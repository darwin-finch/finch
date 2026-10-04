//! SuperGrok credential setup ceremony and add-time device dialog.

use super::*;
use crate::cli::grok_auth::{
    EnsuredGrokCredential, GrokCompensationHandle, GrokCredentialAuthenticator,
    GrokNamedCredentialStart,
};
use crate::providers::{GrokAuthStageError, GrokDeviceEndpointError};

#[derive(Debug, Clone, PartialEq)]
pub(super) enum GrokSetupFailureCause {
    Cancelled,
    Expired,
    Denied,
    StartDisabledOrUnsupported,
    ClientRejected,
    StartTransport,
    PollTransport,
    /// xAI answered a step with an error status. Carries which step and the
    /// status (no secrets), so the dialog says more than "rejected".
    ProviderRejected(String),
    ResponseContract,
    VerificationAuthority,
    IdentityVerification,
    ClientBinding,
    AccountEntitlement,
    Persistence,
    ProtocolOrOther(String),
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
            | GrokDeviceEndpointError::PollRejected(_) => {
                GrokSetupFailureCause::ProviderRejected(endpoint.to_string())
            }
        };
    }
    if let Some(stage) = error.downcast_ref::<GrokAuthStageError>() {
        return match stage {
            GrokAuthStageError::DeviceStartTransport => GrokSetupFailureCause::StartTransport,
            GrokAuthStageError::DevicePollTransport => GrokSetupFailureCause::PollTransport,
            GrokAuthStageError::DeviceStartContract
            | GrokAuthStageError::PollContract
            | GrokAuthStageError::TokenExchangeContract => GrokSetupFailureCause::ResponseContract,
            GrokAuthStageError::TokenExchangeRejected(_) => {
                GrokSetupFailureCause::ProviderRejected(stage.to_string())
            }
            GrokAuthStageError::JwksTransport => GrokSetupFailureCause::VerificationAuthority,
            GrokAuthStageError::IdentityVerification | GrokAuthStageError::IdentitySignature => {
                GrokSetupFailureCause::IdentityVerification
            }
            GrokAuthStageError::ClientBinding => GrokSetupFailureCause::ClientBinding,
            GrokAuthStageError::AccountEntitlement => GrokSetupFailureCause::AccountEntitlement,
        };
    }
    if error
        .downcast_ref::<crate::oauth::OAuthCredentialPersistenceError>()
        .is_some()
    {
        return GrokSetupFailureCause::Persistence;
    }
    GrokSetupFailureCause::ProtocolOrOther(format!("{error:#}"))
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
            "Grok subscription device authorization is disabled or unsupported for this account. No credential was saved. Finch will not invent an OAuth button or switch to Console API-key billing."
        }
        GrokSetupFailureCause::ClientRejected => {
            "xAI rejected Finch as an OAuth client (invalid_client). SuperGrok device login is not available for this independent client. No credential was saved. Finch will not switch to Console API-key billing."
        }
        GrokSetupFailureCause::StartTransport => {
            "Finch could not reach xAI to start Grok subscription sign-in. Check network access and retry. No credential was saved."
        }
        GrokSetupFailureCause::PollTransport => {
            "Finch lost network access while waiting for Grok subscription authorization. Retry for a fresh one-time code. No credential was saved."
        }
        GrokSetupFailureCause::ProviderRejected(ref step) => {
            return format!(
                "Grok subscription sign-in was rejected: {step}. No credential was saved. Finch will not fall back to an API key."
            );
        }
        GrokSetupFailureCause::ResponseContract => {
            "xAI returned an unsupported Grok subscription authorization response. Update Finch before retrying. No credential was saved."
        }
        GrokSetupFailureCause::VerificationAuthority => {
            "Finch could not verify xAI's pinned Grok identity-signing authority. Check network access and retry. No credential was saved."
        }
        GrokSetupFailureCause::IdentityVerification => {
            "Finch could not verify the signed Grok subscription identity. No credential was saved."
        }
        GrokSetupFailureCause::ClientBinding => {
            "The signed Grok identity was not issued for Finch's pinned public client. No credential was saved."
        }
        GrokSetupFailureCause::AccountEntitlement => {
            "The signed Grok identity did not contain a usable subscription account binding. No credential was saved."
        }
        GrokSetupFailureCause::Persistence => {
            "Grok subscription sign-in was validated, but Finch could not save the named credential."
        }
        GrokSetupFailureCause::ProtocolOrOther(ref e) => {
            return format!("Grok subscription sign-in failed: {e}\nNo credential was saved. Finch will not silently switch to API-key billing.");
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
    fn client_rejection_and_missing_device_flow_fail_closed_without_api_key_fallback() {
        let client = grok_setup_failure_summary(GrokSetupFailureCause::ClientRejected);
        assert!(
            client.contains("invalid_client"),
            "independent-client rejection must name invalid_client; summary={client}"
        );
        assert!(
            client.contains("No credential was saved"),
            "OAuth failure must not persist a credential; summary={client}"
        );
        assert!(
            client.contains("will not switch to Console API-key billing"),
            "OAuth failure must not offer Console billing as the next step; summary={client}"
        );
        assert!(
            !client.to_lowercase().contains("use an xai api key"),
            "API keys bill separately and must not be an automatic fallback; summary={client}"
        );
        let missing = grok_setup_failure_summary(GrokSetupFailureCause::StartDisabledOrUnsupported);
        assert!(
            missing.contains("disabled or unsupported"),
            "missing device flow must say so; summary={missing}"
        );
        assert!(
            missing.contains("will not invent") && missing.contains("No credential was saved"),
            "missing device flow must refuse a fake OAuth button and save nothing; summary={missing}"
        );
        assert!(
            missing.contains("Console API-key billing"),
            "missing device flow must refuse Console billing fallback; summary={missing}"
        );
        assert!(
            !missing.contains("console.x.ai"),
            "missing device flow must not steer into Console API keys; summary={missing}"
        );
    }

    /// The reported dialog said only "Grok subscription sign-in was
    /// rejected", which is one message for three different rejections (the
    /// device-code request, the poll, the token exchange). The summary must
    /// say which step xAI rejected and with what status.
    #[test]
    fn test_a_rejected_grok_sign_in_names_the_step_and_status() {
        for (error, step) in [
            (
                anyhow::Error::new(GrokDeviceEndpointError::StartRejected(403)),
                GrokDeviceEndpointError::StartRejected(403).to_string(),
            ),
            (
                anyhow::Error::new(GrokDeviceEndpointError::PollRejected(400)),
                GrokDeviceEndpointError::PollRejected(400).to_string(),
            ),
            (
                anyhow::Error::new(GrokAuthStageError::TokenExchangeRejected(401)),
                GrokAuthStageError::TokenExchangeRejected(401).to_string(),
            ),
        ] {
            let cause = grok_setup_failure_cause(&error);
            assert_eq!(
                cause,
                GrokSetupFailureCause::ProviderRejected(step.clone()),
                "a rejection must keep its step; error={error:#}"
            );
            let summary = grok_setup_failure_summary(cause);
            assert!(
                summary.contains(&step) && summary.contains("HTTP"),
                "the dialog must name the rejected step and its status ({step}); summary={summary:?}"
            );
            assert!(
                summary.contains("No credential was saved")
                    && summary.contains("will not fall back to an API key"),
                "the no-credential and no-API-key-fallback guarantees stay in the message; summary={summary:?}"
            );
        }
    }

    #[test]
    fn typed_grok_failures_keep_distinct_secret_free_actions() {
        let cases = [
            (
                anyhow::Error::new(GrokAuthStageError::DeviceStartTransport),
                GrokSetupFailureCause::StartTransport,
            ),
            (
                anyhow::Error::new(GrokAuthStageError::DevicePollTransport),
                GrokSetupFailureCause::PollTransport,
            ),
            (
                anyhow::Error::new(GrokAuthStageError::PollContract),
                GrokSetupFailureCause::ResponseContract,
            ),
            (
                anyhow::Error::new(GrokAuthStageError::JwksTransport),
                GrokSetupFailureCause::VerificationAuthority,
            ),
            (
                anyhow::Error::new(GrokAuthStageError::IdentitySignature),
                GrokSetupFailureCause::IdentityVerification,
            ),
            (
                anyhow::Error::new(GrokAuthStageError::ClientBinding),
                GrokSetupFailureCause::ClientBinding,
            ),
            (
                anyhow::Error::new(GrokAuthStageError::AccountEntitlement),
                GrokSetupFailureCause::AccountEntitlement,
            ),
            (
                anyhow::Error::new(crate::oauth::OAuthCredentialPersistenceError::Commit),
                GrokSetupFailureCause::Persistence,
            ),
        ];
        let summaries = cases
            .into_iter()
            .map(|(error, expected)| {
                assert_eq!(
                    grok_setup_failure_cause(&error),
                    expected,
                    "typed Grok failure lost its safe stage: {error:#}"
                );
                grok_setup_failure_summary(expected)
            })
            .collect::<Vec<_>>();
        let distinct = summaries.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            distinct.len(),
            summaries.len(),
            "different recovery stages need different actionable summaries: {summaries:?}"
        );
        for summary in summaries {
            assert!(!summary.contains("access-secret"));
            assert!(!summary.contains("refresh-secret"));
            assert!(!summary.contains("id-secret"));
        }
    }
}
