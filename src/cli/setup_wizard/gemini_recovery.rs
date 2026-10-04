//! Google Gemini credential setup ceremony and add-time device dialog.

use super::*;
use crate::cli::gemini_auth::{
    EnsuredGeminiCredential, GeminiCompensationHandle, GeminiCredentialAuthenticator,
    GeminiNamedCredentialStart,
};
use crate::providers::{GeminiAuthStageError, GeminiDeviceEndpointError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GeminiSetupFailureCause {
    Cancelled,
    Expired,
    Denied,
    StartDisabledOrUnsupported,
    ClientRejected,
    /// Google refused a request; the text names the step and HTTP status.
    ProviderRejected(String),
    /// Google refused the token request because Finch sent no client secret.
    ClientSecretMissing(String),
    /// Google answered, but Finch could not accept the response; the text
    /// names the check that failed.
    VerificationFailed(String),
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
                GeminiSetupFailureCause::ProviderRejected(endpoint.to_string())
            }
        };
    }
    if let Some(stage) = error.downcast_ref::<GeminiAuthStageError>() {
        return match stage {
            GeminiAuthStageError::TokenExchangeRejected { .. } => {
                GeminiSetupFailureCause::ProviderRejected(stage.to_string())
            }
            GeminiAuthStageError::ClientSecretMissing { .. } => {
                GeminiSetupFailureCause::ClientSecretMissing(stage.to_string())
            }
            GeminiAuthStageError::PollContract
            | GeminiAuthStageError::TokenExchangeContract
            | GeminiAuthStageError::IdentityVerification
            | GeminiAuthStageError::ClientBinding
            | GeminiAuthStageError::AccountEntitlement => {
                GeminiSetupFailureCause::VerificationFailed(stage.to_string())
            }
        };
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
        GeminiSetupFailureCause::ProviderRejected(ref step) => {
            return format!(
                "Google Gemini subscription sign-in was rejected: {step}. No credential was saved. Finch will not fall back to an API key."
            );
        }
        GeminiSetupFailureCause::ClientSecretMissing(ref step) => {
            return format!(
                "Google Gemini subscription sign-in was rejected: {step}. Set FINCH_GEMINI_CLIENT_SECRET to the secret of the OAuth client Finch signs in with, then retry. No credential was saved. Finch will not fall back to an API key."
            );
        }
        GeminiSetupFailureCause::VerificationFailed(ref step) => {
            return format!(
                "Google Gemini subscription sign-in could not be verified: {step}. No credential was saved. Finch will not fall back to an API key."
            );
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
                            // Through the wizard's own launcher, which is inert
                            // under test: a fixture authorization must never
                            // open a real browser tab.
                            open_browser_silently(&device.verification_uri);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::{OAuthClient, OAuthCredentialStore, OAuthTokenRecord};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use finch_providers::{
        GeminiTokenVerifierProduction, GoogleGeminiOAuthDialect, GOOGLE_PUBLIC_CLIENT_ID,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    const REDIRECT_URI: &str = "http://127.0.0.1:49152/callback";

    #[derive(Default)]
    struct MemoryStore(Mutex<Option<OAuthTokenRecord>>);

    impl OAuthCredentialStore for MemoryStore {
        fn load(&self, _reference: &str) -> anyhow::Result<Option<OAuthTokenRecord>> {
            Ok(self.0.lock().unwrap().clone())
        }

        fn compare_and_swap(
            &self,
            _reference: &str,
            _expected_generation: Option<&str>,
            replacement: &OAuthTokenRecord,
        ) -> anyhow::Result<()> {
            *self.0.lock().unwrap() = Some(replacement.clone());
            Ok(())
        }
    }

    fn authorization_param(authorization_url: &str, name: &str) -> String {
        reqwest::Url::parse(authorization_url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.to_string())
            .unwrap_or_else(|| panic!("authorization URL must carry `{name}`"))
    }

    /// Drive the real browser-flow completion
    /// (`OAuthClient::finish_browser_authorization`) against a fixture token
    /// endpoint, as the wizard does after the loopback page reports success.
    /// The fixture answers only a well-formed authorization-code exchange
    /// that also satisfies `secret_matcher`.
    async fn finish_sign_in(
        server: &mut mockito::ServerGuard,
        secret: Option<&str>,
        secret_matcher: mockito::Matcher,
        token_response: impl FnOnce(&str) -> (usize, String),
    ) -> (anyhow::Result<()>, Arc<MemoryStore>) {
        let origin = server.url();
        let dialect = GoogleGeminiOAuthDialect::new(
            &origin,
            &origin,
            &origin,
            GOOGLE_PUBLIC_CLIENT_ID,
            secret,
            Arc::new(GeminiTokenVerifierProduction),
            true,
        )
        .unwrap();
        let store = Arc::new(MemoryStore::default());
        let client = OAuthClient::new(Arc::new(dialect), store.clone()).unwrap();
        let pending = client
            .begin_browser_authorization(REDIRECT_URI, Duration::from_secs(600))
            .unwrap();
        let state = authorization_param(&pending.authorization_url, "state");
        let nonce = authorization_param(&pending.authorization_url, "nonce");
        let (status, body) = token_response(&nonce);
        let mock = server
            .mock("POST", "/token")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("grant_type".into(), "authorization_code".into()),
                mockito::Matcher::UrlEncoded("redirect_uri".into(), REDIRECT_URI.into()),
                mockito::Matcher::UrlEncoded("code".into(), "fixture-code".into()),
                mockito::Matcher::Regex("code_verifier=".into()),
                secret_matcher,
            ]))
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(body)
            .expect(1)
            .create_async()
            .await;
        let result = client
            .finish_browser_authorization(
                "gemini-sub:default",
                pending,
                &format!("{REDIRECT_URI}?state={state}&code=fixture-code"),
                CancellationToken::new(),
            )
            .await
            .map(|_| ());
        mock.assert_async().await;
        (result, store)
    }

    /// The reported dialog said only "Google Gemini subscription sign-in was
    /// rejected" after the browser reported success. Google's token endpoint
    /// answers a secretless exchange for a desktop client with HTTP 400
    /// `invalid_request` / `client_secret is missing.`; the dialog must name
    /// that step, status and remedy.
    #[tokio::test]
    async fn test_a_secretless_gemini_token_exchange_names_the_step_status_and_remedy() {
        let mut server = mockito::Server::new_async().await;
        let (result, store) = finish_sign_in(&mut server, None, mockito::Matcher::Any, |_| {
            (
                400,
                r#"{"error":"invalid_request","error_description":"client_secret is missing."}"#
                    .to_string(),
            )
        })
        .await;
        let error = result.expect_err("a refused token exchange must fail the sign-in");
        let cause = gemini_setup_failure_cause(&error);
        let summary = gemini_setup_failure_summary(cause.clone());
        assert!(
            matches!(cause, GeminiSetupFailureCause::ClientSecretMissing(_)),
            "a missing-client-secret refusal must keep its own cause; cause={cause:?} error={error:#}"
        );
        assert!(
            summary.contains("authorization-code token exchange")
                && summary.contains("HTTP 400")
                && summary.contains("invalid_request")
                && summary.contains("FINCH_GEMINI_CLIENT_SECRET"),
            "the dialog must name the failing step, its status and the remedy; summary={summary:?}"
        );
        assert!(
            summary.contains("No credential was saved")
                && summary.contains("will not fall back to an API key"),
            "the no-credential and no-API-key-fallback guarantees stay in the message; summary={summary:?}"
        );
        assert!(
            store.0.lock().unwrap().is_none(),
            "a refused exchange must persist nothing"
        );
    }

    #[tokio::test]
    async fn test_a_rejected_gemini_token_exchange_names_the_step_status_and_oauth_code() {
        let mut server = mockito::Server::new_async().await;
        let (result, _store) = finish_sign_in(
            &mut server,
            Some("fixture-secret"),
            mockito::Matcher::UrlEncoded("client_secret".into(), "fixture-secret".into()),
            |_| {
                (
                    400,
                    r#"{"error":"invalid_grant","error_description":"fixture-upstream-text"}"#
                        .to_string(),
                )
            },
        )
        .await;
        let error = result.expect_err("a refused token exchange must fail the sign-in");
        let cause = gemini_setup_failure_cause(&error);
        let summary = gemini_setup_failure_summary(cause.clone());
        assert!(
            matches!(cause, GeminiSetupFailureCause::ProviderRejected(_)),
            "a token-endpoint refusal is a provider rejection; cause={cause:?} error={error:#}"
        );
        assert!(
            summary.contains("authorization-code token exchange")
                && summary.contains("HTTP 400")
                && summary.contains("invalid_grant"),
            "the dialog must name the rejected step, status and OAuth error code; summary={summary:?}"
        );
        assert!(
            !summary.contains("fixture-upstream-text")
                && !summary.contains("fixture-secret")
                && !summary.contains("fixture-code"),
            "the dialog must stay free of upstream text, secrets and codes; summary={summary:?}"
        );
    }

    /// With a client secret configured the real exchange carries it and the
    /// sign-in persists a credential: the fixture only answers a request that
    /// includes `client_secret`.
    #[tokio::test]
    async fn test_gemini_token_exchange_with_a_client_secret_completes_and_persists() {
        let mut server = mockito::Server::new_async().await;
        let (result, store) = finish_sign_in(
            &mut server,
            Some("fixture-secret"),
            mockito::Matcher::UrlEncoded("client_secret".into(), "fixture-secret".into()),
            |nonce| {
                let encode =
                    |value: serde_json::Value| URL_SAFE_NO_PAD.encode(value.to_string().as_bytes());
                let id_token = format!(
                    "{}.{}.{}",
                    encode(serde_json::json!({"alg": "RS256", "typ": "JWT"})),
                    encode(serde_json::json!({
                        "iss": "https://accounts.google.com",
                        "aud": GOOGLE_PUBLIC_CLIENT_ID,
                        "sub": "fixture-subject",
                        "email": "user@example.com",
                        "nonce": nonce,
                        "exp": chrono::Utc::now().timestamp() + 3600,
                    })),
                    URL_SAFE_NO_PAD.encode(b"fixture-signature"),
                );
                (
                    200,
                    serde_json::json!({
                        "access_token": "fixture-access",
                        "refresh_token": "fixture-refresh",
                        "id_token": id_token,
                        "expires_in": 3600,
                    })
                    .to_string(),
                )
            },
        )
        .await;
        let dialog = result
            .as_ref()
            .err()
            .map(|error| gemini_setup_failure_summary(gemini_setup_failure_cause(error)));
        assert!(
            result.is_ok(),
            "an exchange carrying the client secret must complete; dialog={dialog:?} error={:?}",
            result.as_ref().err().map(|error| format!("{error:#}"))
        );
        let account = store
            .0
            .lock()
            .unwrap()
            .as_ref()
            .map(|record| record.account.clone());
        assert_eq!(
            account.as_deref(),
            Some("user@example.com"),
            "the verified account must be persisted by a completed sign-in"
        );
    }

    /// Every post-callback stage used to collapse into "was rejected"; each
    /// must now say which check failed.
    #[test]
    fn test_gemini_response_check_failures_name_the_failed_check() {
        for stage in [
            GeminiAuthStageError::TokenExchangeContract,
            GeminiAuthStageError::IdentityVerification,
            GeminiAuthStageError::ClientBinding,
            GeminiAuthStageError::AccountEntitlement,
        ] {
            let error = anyhow::Error::new(stage).context("Gemini sign-in did not complete");
            let cause = gemini_setup_failure_cause(&error);
            let summary = gemini_setup_failure_summary(cause.clone());
            assert_eq!(
                cause,
                GeminiSetupFailureCause::VerificationFailed(stage.to_string()),
                "a failed response check must keep its stage; error={error:#}"
            );
            assert!(
                summary.contains(&stage.to_string()) && !summary.contains("was rejected"),
                "the dialog must name the failed check and not call it a rejection; summary={summary:?}"
            );
        }
    }
}
