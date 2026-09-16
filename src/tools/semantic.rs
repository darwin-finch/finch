//! Finch-side semantic tool catalog for provider binding compilation.
//!
//! Provider adapters compile [`finch_providers::SemanticTool`] values into
//! wire bindings. This module is the application catalog: stable identities,
//! declared authority, handler presence, and permission grants. Native
//! provider tools are advertised only when both a handler and a grant exist.

use crate::programs::ExecutionEffect;
use crate::tools::permissions::PermissionManager;
use crate::tools::registry::ToolRegistry;
use crate::tools::types::ToolDefinition;
use finch_providers::{
    NativeToolGrant, SemanticTool, ToolAuthority, ToolCompilePolicy, ToolOrigin,
};

/// Map a declared execution effect onto the provider-neutral authority class.
pub fn tool_authority_from_effect(effect: ExecutionEffect) -> ToolAuthority {
    match effect {
        ExecutionEffect::Pure => ToolAuthority::Pure,
        ExecutionEffect::VmRead => ToolAuthority::VmRead,
        ExecutionEffect::VmWrite => ToolAuthority::VmWrite,
        ExecutionEffect::WorkspaceRead => ToolAuthority::WorkspaceRead,
        ExecutionEffect::ExternalRead => ToolAuthority::ExternalRead,
        ExecutionEffect::WorkspaceWrite => ToolAuthority::WorkspaceWrite,
        ExecutionEffect::ExternalWrite => ToolAuthority::ExternalWrite,
        ExecutionEffect::Destructive => ToolAuthority::Destructive,
        ExecutionEffect::Unclassified => ToolAuthority::Unclassified,
    }
}

/// Compile policy for the registered tools. Native grants stay empty unless
/// [`semantic_tools_for_advertisement`] includes a granted native candidate.
pub fn compile_policy_from_registry(
    registry: &ToolRegistry,
    permissions: &PermissionManager,
) -> ToolCompilePolicy {
    let mut policy = ToolCompilePolicy::new();
    for name in registry.tool_names() {
        if !permissions.allows_advertising(&name) {
            continue;
        }
        policy.authority.insert(
            name.clone(),
            tool_authority_from_effect(registry.declared_effect(&name)),
        );
    }
    policy
}

/// Semantic tools Finch may advertise this turn.
///
/// `definitions` are the Finch-owned schemas (built-in + MCP). Native
/// provider candidates are included only when a handler is registered and
/// [`PermissionManager::allows_advertising`] is true. A provider cannot
/// invent an executable namespace merely by returning one.
pub fn semantic_tools_for_advertisement(
    definitions: &[ToolDefinition],
    registry: &ToolRegistry,
    permissions: &PermissionManager,
    native_candidates: &[NativeToolGrant],
) -> Vec<SemanticTool> {
    let mut tools = Vec::new();
    for definition in definitions {
        let has_handler =
            registry.has_tool(&definition.name) || definition.name.starts_with("mcp_");
        if !has_handler || !permissions.allows_advertising(&definition.name) {
            continue;
        }
        tools.push(
            SemanticTool::finch(
                definition.name.clone(),
                definition.description.clone(),
                definition.input_schema.clone(),
            )
            .with_authority(tool_authority_from_effect(
                registry.declared_effect(&definition.name),
            )),
        );
    }
    for grant in native_candidates {
        let has_handler = registry.has_tool(&grant.semantic_identity);
        let granted = permissions.allows_advertising(&grant.semantic_identity);
        if !has_handler || !granted {
            continue;
        }
        let Some(tool) = registry.get(&grant.semantic_identity) else {
            continue;
        };
        let mut semantic = SemanticTool::finch(
            grant.semantic_identity.clone(),
            tool.description().to_string(),
            tool.input_schema(),
        )
        .with_authority(tool_authority_from_effect(tool.effect()))
        .provider_native(grant.wire_name.clone(), grant.namespace.clone());
        semantic.origin = ToolOrigin::ProviderNative {
            wire_name: grant.wire_name.clone(),
            namespace: grant.namespace.clone(),
        };
        semantic.available = true;
        semantic.granted = true;
        if tools
            .iter()
            .any(|existing| existing.identity == semantic.identity)
        {
            continue;
        }
        tools.push(semantic);
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::permissions::{PermissionRule, ToolPermissionConfig};
    use crate::tools::Tool;
    use crate::tools::{ToolContext, ToolInputSchema};
    use anyhow::Result;
    use async_trait::async_trait;
    use finch_providers::WireProtocol;
    use serde_json::Value;

    struct StubSearch;

    #[async_trait]
    impl Tool for StubSearch {
        fn name(&self) -> &str {
            "web_search"
        }
        fn effect(&self) -> ExecutionEffect {
            ExecutionEffect::ExternalRead
        }
        fn description(&self) -> &str {
            "search"
        }
        fn input_schema(&self) -> ToolInputSchema {
            ToolInputSchema::simple(vec![("query", "q")])
        }
        async fn execute(&self, _input: Value, _context: &ToolContext<'_>) -> Result<String> {
            Ok("ok".into())
        }
    }

    fn grant() -> NativeToolGrant {
        NativeToolGrant {
            protocol: WireProtocol::OpenAiChatGptResponsesLite,
            semantic_identity: "web_search".into(),
            wire_name: "web_search".into(),
            namespace: Some("web".into()),
        }
    }

    #[test]
    fn test_native_tool_absent_without_handler() {
        let registry = ToolRegistry::new();
        let permissions = PermissionManager::new();
        let advertised = semantic_tools_for_advertisement(&[], &registry, &permissions, &[grant()]);
        assert!(
            advertised.is_empty(),
            "native tool without a Finch handler must be absent: {advertised:?}"
        );
    }

    #[test]
    fn test_native_tool_absent_without_grant() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(StubSearch));
        let mut permissions = PermissionManager::new();
        permissions.register_tool_config(
            "web_search".into(),
            ToolPermissionConfig {
                enabled: false,
                rule: PermissionRule::Allow,
                allowed_patterns: Vec::new(),
                blocked_patterns: Vec::new(),
            },
        );
        let advertised = semantic_tools_for_advertisement(&[], &registry, &permissions, &[grant()]);
        assert!(
            advertised.is_empty(),
            "native tool without an advertising grant must be absent: {advertised:?}"
        );
    }

    #[test]
    fn test_native_tool_present_with_handler_and_grant() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(StubSearch));
        let permissions = PermissionManager::new();
        let advertised = semantic_tools_for_advertisement(&[], &registry, &permissions, &[grant()]);
        assert_eq!(advertised.len(), 1, "{advertised:?}");
        assert_eq!(advertised[0].identity, "web_search");
        assert!(advertised[0].available && advertised[0].granted);
        assert!(matches!(
            advertised[0].origin,
            ToolOrigin::ProviderNative { ref wire_name, .. } if wire_name == "web_search"
        ));
    }
}
