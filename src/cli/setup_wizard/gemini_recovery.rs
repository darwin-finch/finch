//! Google Gemini credential setup ceremony and add-time device dialog.

use super::*;
use crate::cli::gemini_auth::{
    EnsuredGeminiCredential, GeminiCompensationHandle, GeminiCredentialAuthenticator,
    GeminiNamedCredentialStart,
};
use crate::providers::{GeminiAuthStageError, GeminiDeviceEndpointError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeminiSetupFailureCause {
    Cancelled,
    Expired,
    Denied,
    StartDisabledOrUnsupported,
    ClientRejected,
    ProviderRejected,
    Persistence,
    ProtocolOrOther,
}

#[allow(dead_code)]
pub(super) fn gemini_setup_references(result: &SetupResult) -> std::collections::BTreeSet<String> {
    result
        .providers
        .iter()
        .filter_map(|provider| match provider {
            ProviderEntry::Credentialed {
                provider: crate::config::CredentialProvider::GeminiSubscription,
                credential,
                ..
            } => Some(credential.credential_ref.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn gemini_setup_failure_cause(error: &anyhow::Error) -> GeminiSetupFailureCause {
    if let Some(terminal) = error.downcast_ref::<crate::oauth::OAuthDeviceAuthorizationError>() {
        return match terminal {
            crate::oauth::OAuthDeviceAuthorizationError::Cancelled => {
                GeminiSetupFailureCause::Cancelled
            }
            crate::oauth::OAuthDeviceAuthorizationError::Expired => {
                GeminiSetupFailureCause::Expired
            }
            crate::oauth::OAuthDeviceAuthorizationError::Denied => GeminiSetupFailureCause::Denied,
        };
    }
    if let Some(endpoint) = error.downcast_ref::<GeminiDeviceEndpointError>() {
        return match endpoint {
            GeminiDeviceEndpointError::StartDisabledOrUnsupported => {
                GeminiSetupFailureCause::StartDisabledOrUnsupported
            }
            GeminiDeviceEndpointError::ClientRejected => GeminiSetupFailureCause::ClientRejected,
            GeminiDeviceEndpointError::StartRejected(_)
            | GeminiDeviceEndpointError::PollRejected(_) => {
                GeminiSetupFailureCause::ProviderRejected
            }
        };
    }
    if error.downcast_ref::<GeminiAuthStageError>().is_some() {
        return GeminiSetupFailureCause::ProviderRejected;
    }
    if error
        .downcast_ref::<crate::oauth::OAuthCredentialPersistenceError>()
        .is_some()
    {
        return GeminiSetupFailureCause::Persistence;
    }
    GeminiSetupFailureCause::ProtocolOrOther
}

pub(super) fn gemini_setup_failure_summary(cause: GeminiSetupFailureCause) -> String {
    match cause {
        GeminiSetupFailureCause::Cancelled => {
            "Google Gemini subscription sign-in was cancelled. No credential was saved."
        }
        GeminiSetupFailureCause::Expired => {
            "Google Gemini subscription sign-in expired. No credential was saved. Retry for a fresh one-time code."
        }
        GeminiSetupFailureCause::Denied => {
            "Google Gemini subscription sign-in was denied. No credential was saved."
        }
        GeminiSetupFailureCause::StartDisabledOrUnsupported => {
            "Google Gemini subscription authorization is disabled or unsupported for this account. No credential was saved. Finch will not switch to AI Studio API-key billing."
        }
        GeminiSetupFailureCause::ClientRejected => {
            "Google rejected Finch as an OAuth client (invalid_client). Verify your Google OAuth client configuration."
        }
        GeminiSetupFailureCause::ProviderRejected => {
            "Google Gemini subscription sign-in was rejected. No credential was saved. Finch will not fall back to an API key."
        }
        GeminiSetupFailureCause::Persistence => {
            "Google Gemini subscription sign-in was validated, but Finch could not save the named credential."
        }
        GeminiSetupFailureCause::ProtocolOrOther => {
            "Google Gemini subscription sign-in failed. No credential was saved. Finch will not silently switch to API-key billing."
        }
    }
    .into()
}

pub(super) fn gemini_persisted_reference(persisted: Option<&ProviderEntry>) -> String {
    match persisted {
        Some(ProviderEntry::Credentialed {
            provider: crate::config::CredentialProvider::GeminiSubscription,
            credential,
            ..
        }) => credential.credential_ref.clone(),
        _ => "gemini-sub:default".to_string(),
    }
}

pub(super) fn spawn_add_time_gemini_device_flow(
    authenticator: std::sync::Arc<dyn GeminiCredentialAuthenticator>,
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
                        Ok(GeminiNamedCredentialStart::Ensured(ensured)) => Ok(ensured.credential),
                        Ok(GeminiNamedCredentialStart::AuthorizationRequired(device)) => {
                            *pending.lock().unwrap() = Some(DeviceAuthPresentation {
                                verification_uri: device.verification_uri.clone(),
                                user_code: device.user_code.clone(),
                                expires_in: device.expires_in,
                            });
                            let _ = crate::cli::gemini_auth::open_browser(&device.verification_uri);
                            authenticator
                                .finish_named_credential(&reference, &device, cancel)
                                .await
                                .map(|ensured: EnsuredGeminiCredential| ensured.credential)
                        }
                        Err(error) => Err(error),
                    }
                }),
                Err(error) => Err(anyhow::Error::new(error)
                    .context("Google Gemini sign-in could not start in setup")),
            };
        *outcome.lock().unwrap() = Some(result);
    });
}

#[allow(dead_code)]
pub(super) fn compensate_gemini_setup<A>(
    authenticator: &A,
    handles: &[GeminiCompensationHandle],
) -> Vec<String>
where
    A: GeminiCredentialAuthenticator,
{
    let mut failed = Vec::new();
    for handle in handles.iter().rev() {
        if authenticator.compensate_with_tombstone(handle).is_err() {
            failed.push(handle.reference().to_string());
        }
    }
    failed
}
