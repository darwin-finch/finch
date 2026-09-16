// Non-overridable dispatch boundary: tokens that prove a request was validated.

use super::*;
use std::sync::Arc;

use crate::tool_bindings::{compile_from_definitions, ToolBindingTable};

/// A request whose effective provider/model identity and optional
/// capabilities were checked by Finch's non-overridable dispatch boundary.
///
/// The fields and constructor are private to this module, so even a
/// provider backend elsewhere in the crate cannot fabricate a token.
///
/// ```compile_fail
/// use finch_providers::ValidatedProviderRequest;
///
/// let _ = ValidatedProviderRequest {
///     request: panic!("unreachable"),
///     capabilities: panic!("unreachable"),
/// };
/// ```
pub struct ValidatedProviderRequest {
    request: ProviderRequest,
    capabilities: ModelCapabilities,
    tool_bindings: Arc<ToolBindingTable>,
    target: usize,
    target_type: TypeId,
}

impl ValidatedProviderRequest {
    /// Consume this token at the exact provider instance for which it was
    /// validated and return the effective request plus the immutable
    /// tool-binding table compiled for it.
    #[doc(hidden)]
    pub fn into_request_for(
        self,
        provider: &(impl ProviderBackend + ?Sized),
    ) -> Result<(ProviderRequest, Arc<ToolBindingTable>)> {
        if self.target != provider_target(provider)
            || self.target_type != ProviderConcreteType::provider_concrete_type_id(provider)
        {
            anyhow::bail!(
                "Validated provider request was presented to a different provider instance or concrete backend type"
            );
        }
        Ok((self.request, self.tool_bindings))
    }

    /// The exact descriptor used to validate this request.
    pub fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    /// Immutable tool-binding table compiled for this validated request.
    pub fn tool_bindings(&self) -> &Arc<ToolBindingTable> {
        &self.tool_bindings
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
    let tool_bindings = compile_validated_bindings(provider.name(), &effective, &capabilities)?;
    Ok(ValidatedProviderRequest {
        request: effective,
        capabilities,
        tool_bindings,
        target: provider_target(provider),
        target_type: ProviderConcreteType::provider_concrete_type_id(provider),
    })
}

fn compile_validated_bindings(
    provider: &str,
    request: &ProviderRequest,
    capabilities: &ModelCapabilities,
) -> Result<Arc<ToolBindingTable>> {
    let definitions = request.tools.as_deref().unwrap_or_default();
    if definitions.is_empty() {
        let protocol = capabilities
            .wire_protocol
            .protocol
            .unwrap_or(WireProtocol::AnthropicMessages);
        return Ok(Arc::new(ToolBindingTable::empty(
            protocol,
            provider,
            &request.model,
        )));
    }
    let protocol = capabilities
        .wire_protocol
        .protocol
        .ok_or_else(|| anyhow::anyhow!("cannot compile tool bindings: wire protocol is unknown"))?;
    let table = compile_from_definitions(
        protocol,
        provider,
        &request.model,
        definitions,
        request.tool_policy(),
    )?;
    Ok(Arc::new(table))
}

fn provider_target(provider: &(impl ProviderBackend + ?Sized)) -> usize {
    provider as *const _ as *const () as usize
}
