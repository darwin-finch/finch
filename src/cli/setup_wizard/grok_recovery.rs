//! SuperGrok credential setup ceremony and add-time device dialog.

use super::*;
use crate::cli::grok_auth::{
    EnsuredGrokCredential, GrokCompensationHandle, GrokCredentialAuthenticator,
    GrokNamedCredentialStart,
};
use crate::providers::{GrokAuthStageError, GrokDeviceEndpointError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GrokSetupFailureCause {
    Cancelled,
    Expired,
    Denied,
    StartDisabledOrUnsupported,
    ClientRejected,
    ProviderRejected,
    Persistence,
    ProtocolOrOther,
}

pub(super) fn grok_setup_references(result: &SetupResult) -> std::collections::BTreeSet<String> {
    result
        .providers
        .iter()
        .filter_map(|provider| match provider {
            ProviderEntry::Credentialed {
                provider: crate::config::CredentialProvider::GrokSubscription,
                credential,
                ..
            } => Some(credential.credential_ref.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn grok_setup_failure_cause(error: &anyhow::Error) -> GrokSetupFailureCause {
    if let Some(terminal) = error.downcast_ref::<crate::oauth::OAuthDeviceAuthorizationError>() {
        return match terminal {
            crate::oauth::OAuthDeviceAuthorizationError::Cancelled => {
                GrokSetupFailureCause::Cancelled
            }
            crate::oauth::OAuthDeviceAuthorizationError::Expired => GrokSetupFailureCause::Expired,
            crate::oauth::OAuthDeviceAuthorizationError::Denied => GrokSetupFailureCause::Denied,
        };
    }
    if let Some(endpoint) = error.downcast_ref::<GrokDeviceEndpointError>() {
        return match endpoint {
            GrokDeviceEndpointError::StartDisabledOrUnsupported => {
                GrokSetupFailureCause::StartDisabledOrUnsupported
            }
            GrokDeviceEndpointError::ClientRejected => GrokSetupFailureCause::ClientRejected,
            GrokDeviceEndpointError::StartRejected(_)
            | GrokDeviceEndpointError::PollRejected(_) => GrokSetupFailureCause::ProviderRejected,
        };
    }
    if error.downcast_ref::<GrokAuthStageError>().is_some() {
        return GrokSetupFailureCause::ProviderRejected;
    }
    if error
        .downcast_ref::<crate::oauth::OAuthCredentialPersistenceError>()
        .is_some()
    {
        return GrokSetupFailureCause::Persistence;
    }
    GrokSetupFailureCause::ProtocolOrOther
}

pub(super) fn grok_setup_failure_summary(cause: GrokSetupFailureCause) -> String {
    match cause {
        GrokSetupFailureCause::Cancelled => {
            "Grok subscription sign-in was cancelled. No credential was saved."
        }
        GrokSetupFailureCause::Expired => {
            "Grok subscription sign-in expired. No credential was saved. Retry for a fresh one-time code."
        }
        GrokSetupFailureCause::Denied => {
            "Grok subscription sign-in was denied. No credential was saved."
        }
        GrokSetupFailureCause::StartDisabledOrUnsupported => {
            "Grok subscription device authorization is disabled or unsupported for this account. Finch will not invent an OAuth button. Add the separate Grok API-key provider from console.x.ai if you want Console billing."
        }
        GrokSetupFailureCause::ClientRejected => {
            "xAI rejected Finch as an OAuth client (invalid_client). SuperGrok device login is not available for this independent client. Use an xAI API key from console.x.ai instead. Finch will not silently switch credentials."
        }
        GrokSetupFailureCause::ProviderRejected => {
            "Grok subscription sign-in was rejected. No credential was saved. Finch will not fall back to an API key."
        }
        GrokSetupFailureCause::Persistence => {
            "Grok subscription sign-in was validated, but Finch could not save the named credential."
        }
        GrokSetupFailureCause::ProtocolOrOther => {
            "Grok subscription sign-in failed. No credential was saved. Finch will not silently switch to API-key billing."
        }
    }
    .into()
}

pub(super) fn grok_persisted_reference(persisted: Option<&ProviderEntry>) -> String {
    match persisted {
        Some(ProviderEntry::Credentialed {
            provider: crate::config::CredentialProvider::GrokSubscription,
            credential,
            ..
        }) => credential.credential_ref.clone(),
        _ => "grok-sub:default".to_string(),
    }
}

pub(super) fn spawn_add_time_grok_device_flow(
    authenticator: std::sync::Arc<dyn GrokCredentialAuthenticator>,
    reference: String,
    pending: std::sync::Arc<std::sync::Mutex<Option<DeviceAuthPresentation>>>,
    outcome: DeviceAuthOutcome,
    cancel: tokio_util::sync::CancellationToken,
) {
    std::thread::spawn(move || {
        let result =
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(async {
                    match authenticator
                        .begin_named_credential(&reference, cancel.clone())
                        .await
                    {
                        Ok(GrokNamedCredentialStart::Ensured(ensured)) => Ok(ensured.credential),
                        Ok(GrokNamedCredentialStart::AuthorizationRequired(device)) => {
                            *pending.lock().unwrap() = Some(DeviceAuthPresentation {
                                verification_uri: device.verification_uri.clone(),
                                user_code: device.user_code.clone(),
                                expires_in: device.expires_in,
                            });
                            authenticator
                                .finish_named_credential(&reference, &device, cancel)
                                .await
                                .map(|ensured: EnsuredGrokCredential| ensured.credential)
                        }
                        Err(error) => Err(error),
                    }
                }),
                Err(error) => Err(anyhow::Error::new(error)
                    .context("Grok device sign-in could not start in setup")),
            };
        *outcome.lock().unwrap() = Some(result);
    });
}

pub(super) fn compensate_grok_setup<A>(
    authenticator: &A,
    handles: &[GrokCompensationHandle],
) -> Vec<String>
where
    A: GrokCredentialAuthenticator,
{
    let mut failed = Vec::new();
    for handle in handles.iter().rev() {
        if authenticator.compensate_with_tombstone(handle).is_err() {
            failed.push(handle.reference().to_string());
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_rejection_and_missing_device_flow_offer_api_key_instead_of_fake_oauth() {
        let client = grok_setup_failure_summary(GrokSetupFailureCause::ClientRejected);
        assert!(client.contains("invalid_client") || client.contains("API key"));
        assert!(client.contains("will not silently"));
        let missing = grok_setup_failure_summary(GrokSetupFailureCause::StartDisabledOrUnsupported);
        assert!(missing.contains("API-key") || missing.contains("console.x.ai"));
        assert!(missing.contains("will not invent") || missing.contains("disabled"));
    }
}
