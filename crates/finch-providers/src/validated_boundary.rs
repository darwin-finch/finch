// Non-overridable dispatch boundary: tokens that prove a request was validated.

use super::*;

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
