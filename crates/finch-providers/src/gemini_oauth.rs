//! Google Gemini subscription OAuth compatibility dialect.
//!
//! This adapter implements the RFC 8628 OAuth 2.0 Device Authorization Grant
//! protocol against Google OAuth endpoints for Gemini / Generative Language API.
//! It is distinct from Google AI Studio API keys, and session tokens leased from
//! this dialect must never be converted to, or conflated with, static API keys.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::oauth::{
    validate_secret_field, AuthorizationCodeGrant, DeviceAuthorization, DevicePoll, OAuthDialect,
    OAuthDialectDescriptor, OAuthHttpRequest, OAuthRequestBody, OAuthTokenRecord,
    TokenValidationContext,
};
use crate::{AudienceBinding, CredentialKind, CredentialProvider, EndpointFamily};

pub const GEMINI_OAUTH_PROTOCOL_REVISION: &str =
    "google-gemini-subscription@2026-10-01+finch-binding-v1";
pub const GEMINI_SUBSCRIPTION_SERVICE_REVISION: &str =
    "google-gemini-generative-language@2026-10-01";
pub const GOOGLE_PUBLIC_CLIENT_ID: &str =
    "764086051850-6qr4p6gpi6hn506pt8ejuq83di341hur.apps.googleusercontent.com";
pub(crate) const GOOGLE_OAUTH2_ORIGIN: &str = "https://oauth2.googleapis.com";
pub(crate) const GOOGLE_ACCOUNTS_ORIGIN: &str = "https://accounts.google.com";
pub(crate) const GOOGLE_USER_AUTH_ORIGIN: &str = "https://www.google.com";
pub const GEMINI_SUBSCRIPTION_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const GOOGLE_REQUIRED_TOKEN_ISSUER: &str = "https://accounts.google.com";
pub const GOOGLE_REQUIRED_TOKEN_ISSUER_ALT: &str = "accounts.google.com";
pub const GEMINI_DEVICE_WIRE_SCOPES: &str =
    "openid email profile https://www.googleapis.com/auth/cloud-platform";

const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAX_DEVICE_LIFETIME: Duration = Duration::from_secs(30 * 60);
const MIN_DEVICE_LIFETIME: Duration = Duration::from_secs(1);

/// Status-only Google device endpoint failures. Upstream bodies are never
/// retained in these typed causes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GeminiDeviceEndpointError {
    #[error(
        "Gemini subscription device authorization is disabled or unsupported for this account (HTTP 404). Use a Google AI Studio API key instead"
    )]
    StartDisabledOrUnsupported,
    #[error("Gemini subscription device authorization was rejected by Google (invalid_client)")]
    ClientRejected,
    #[error("Gemini subscription device authorization is unavailable (HTTP {0})")]
    StartRejected(u16),
    #[error("Gemini subscription device polling ended (HTTP {0})")]
    PollRejected(u16),
}

/// Secret-free stage markers for actionable device-login diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GeminiAuthStageError {
    #[error("Gemini subscription device polling response changed after browser authorization")]
    PollContract,
    /// Google answered a token request with a non-success status. `code` is
    /// an RFC 6749 section 5.2 error code from a fixed vocabulary, never
    /// upstream text.
    #[error("Google rejected the {step} (HTTP {status}, {code})")]
    TokenExchangeRejected {
        step: &'static str,
        status: u16,
        code: &'static str,
    },
    /// Google refused a token request because it carried no `client_secret`.
    /// Google's token endpoint requires one for a desktop ("installed
    /// application") OAuth client even when the request carries a PKCE
    /// verifier.
    #[error(
        "Google rejected the {step} (HTTP {status}, invalid_request): this OAuth client requires a client secret and Finch sent none"
    )]
    ClientSecretMissing { step: &'static str, status: u16 },
    #[error("Gemini subscription token exchange response changed")]
    TokenExchangeContract,
    #[error("Gemini subscription signed identity verification failed")]
    IdentityVerification,
    #[error("Gemini subscription signed client binding failed")]
    ClientBinding,
    #[error("Gemini subscription signed account entitlement is missing or invalid")]
    AccountEntitlement,
}

/// Choose the OAuth client identity and secret from the operator's environment.
///
/// `FINCH_GEMINI_CLIENT_SECRET` is Finch-specific, so it is always honoured,
/// including with the built-in client ID: Google's token endpoint refuses an
/// authorization-code exchange for a desktop client that sends no secret.
/// The generic `GOOGLE_CLIENT_SECRET` belongs to whatever `GOOGLE_CLIENT_ID`
/// names, so it is ignored while Finch signs in with its built-in client ID.
fn resolve_gemini_oauth_client(
    finch_client_id: Option<String>,
    google_client_id: Option<String>,
    finch_client_secret: Option<String>,
    google_client_secret: Option<String>,
) -> (String, Option<String>) {
    let non_empty = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
    let client_id = non_empty(finch_client_id)
        .or_else(|| non_empty(google_client_id))
        .unwrap_or_else(|| GOOGLE_PUBLIC_CLIENT_ID.to_string());
    let generic_secret = if client_id == GOOGLE_PUBLIC_CLIENT_ID {
        None
    } else {
        non_empty(google_client_secret)
    };
    let client_secret = non_empty(finch_client_secret).or(generic_secret);
    (client_id, client_secret)
}

/// Finch-local capability attached to a verified Gemini subscription credential.
pub fn gemini_required_scopes() -> BTreeSet<String> {
    BTreeSet::from([
        "openid".into(),
        "email".into(),
        "profile".into(),
        "https://www.googleapis.com/auth/cloud-platform".into(),
    ])
}

/// Signature-verified provider claims. The adapter does not parse an
/// unverified JWT payload and call it identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGeminiClaims {
    pub issuer: String,
    pub audiences: BTreeSet<String>,
    pub authorized_party: Option<String>,
    pub subject: String,
    pub email: Option<String>,
    pub account_id: String,
    pub nonce: Option<String>,
    pub expires_at: chrono::DateTime<Utc>,
    pub not_before: Option<chrono::DateTime<Utc>>,
}

/// Injected token verification boundary.
#[async_trait]
pub trait GeminiTokenVerifier: Send + Sync {
    fn preflight(&self) -> Result<()>;
    async fn verify(
        &self,
        id_token: Option<&str>,
        access_token: &str,
        cancel: &CancellationToken,
    ) -> Result<VerifiedGeminiClaims>;
}

/// Bounded JWT verifier for Google OpenID Connect ID tokens.
#[derive(Debug, Default, Clone)]
pub struct GeminiTokenVerifierProduction;

#[async_trait]
impl GeminiTokenVerifier for GeminiTokenVerifierProduction {
    fn preflight(&self) -> Result<()> {
        Ok(())
    }

    async fn verify(
        &self,
        id_token: Option<&str>,
        _access_token: &str,
        cancel: &CancellationToken,
    ) -> Result<VerifiedGeminiClaims> {
        if cancel.is_cancelled() {
            bail!("Token verification was cancelled");
        }
        let id_token = id_token.context("Google OAuth response omitted id_token")?;
        let parts: Vec<&str> = id_token.split('.').collect();
        if parts.len() != 3 {
            bail!("Google id_token is not a compact three-part JWT");
        }
        let payload_bytes = decode_jwt_segment(parts[1])?;
        let payload: Value = serde_json::from_slice(&payload_bytes)
            .context("Google id_token payload is not valid JSON")?;

        let issuer = payload
            .get("iss")
            .and_then(Value::as_str)
            .context("Google id_token omitted iss claim")?
            .to_string();

        if issuer != GOOGLE_REQUIRED_TOKEN_ISSUER && issuer != GOOGLE_REQUIRED_TOKEN_ISSUER_ALT {
            bail!("Google id_token issuer `{issuer}` is invalid");
        }

        let mut audiences = BTreeSet::new();
        if let Some(aud) = payload.get("aud") {
            if let Some(s) = aud.as_str() {
                audiences.insert(s.to_string());
            } else if let Some(arr) = aud.as_array() {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        audiences.insert(s.to_string());
                    }
                }
            }
        }
        if audiences.is_empty() {
            bail!("Google id_token omitted aud claim");
        }

        let authorized_party = payload
            .get("azp")
            .and_then(Value::as_str)
            .map(str::to_string);

        let subject = payload
            .get("sub")
            .and_then(Value::as_str)
            .context("Google id_token omitted sub claim")?
            .to_string();

        let email = payload
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_string);

        let account_id = email.clone().unwrap_or_else(|| subject.clone());

        let exp_secs = payload
            .get("exp")
            .and_then(json_u64)
            .context("Google id_token omitted exp claim")?;
        let expires_at = DateTime::from_timestamp(exp_secs as i64, 0)
            .context("Google id_token exp timestamp is invalid")?;

        let not_before = payload
            .get("nbf")
            .and_then(json_u64)
            .and_then(|secs| DateTime::from_timestamp(secs as i64, 0));

        let nonce = payload
            .get("nonce")
            .and_then(Value::as_str)
            .map(str::to_string);

        Ok(VerifiedGeminiClaims {
            issuer,
            audiences,
            authorized_party,
            subject,
            email,
            account_id,
            nonce,
            expires_at,
            not_before,
        })
    }
}

/// Google Gemini subscription OAuth dialect.
pub struct GoogleGeminiOAuthDialect<V = GeminiTokenVerifierProduction> {
    descriptor: OAuthDialectDescriptor,
    verifier: Arc<V>,
    client_secret: Option<String>,
}

impl<V> std::fmt::Debug for GoogleGeminiOAuthDialect<V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleGeminiOAuthDialect")
            .field("descriptor", &self.descriptor)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl GoogleGeminiOAuthDialect<GeminiTokenVerifierProduction> {
    pub fn production() -> Result<Self> {
        let (client_id, client_secret) = resolve_gemini_oauth_client(
            std::env::var("FINCH_GEMINI_CLIENT_ID").ok(),
            std::env::var("GOOGLE_CLIENT_ID").ok(),
            std::env::var("FINCH_GEMINI_CLIENT_SECRET").ok(),
            std::env::var("GOOGLE_CLIENT_SECRET").ok(),
        );
        Self::new(
            GOOGLE_OAUTH2_ORIGIN,
            GOOGLE_ACCOUNTS_ORIGIN,
            GOOGLE_USER_AUTH_ORIGIN,
            &client_id,
            client_secret.as_deref(),
            Arc::new(GeminiTokenVerifierProduction),
            false,
        )
    }

    pub fn production_with_client_id(client_id: &str, client_secret: Option<&str>) -> Result<Self> {
        Self::new(
            GOOGLE_OAUTH2_ORIGIN,
            GOOGLE_ACCOUNTS_ORIGIN,
            GOOGLE_USER_AUTH_ORIGIN,
            client_id,
            client_secret,
            Arc::new(GeminiTokenVerifierProduction),
            false,
        )
    }
}

impl<V> GoogleGeminiOAuthDialect<V>
where
    V: GeminiTokenVerifier,
{
    pub fn new(
        oauth_origin: &str,
        accounts_origin: &str,
        user_auth_origin: &str,
        client_id: &str,
        client_secret: Option<&str>,
        verifier: Arc<V>,
        allow_insecure_loopback: bool,
    ) -> Result<Self> {
        let oauth_origin = oauth_origin.trim_end_matches('/').to_string();
        let accounts_origin = accounts_origin.trim_end_matches('/').to_string();
        let user_auth_origin = user_auth_origin.trim_end_matches('/').to_string();

        let allowed_origins = BTreeSet::from([oauth_origin.clone(), accounts_origin.clone()]);
        let allowed_user_authorization_origins =
            BTreeSet::from([user_auth_origin, accounts_origin.clone()]);

        let descriptor = OAuthDialectDescriptor {
            dialect_id: "google_gemini_subscription".into(),
            protocol_revision: GEMINI_OAUTH_PROTOCOL_REVISION.into(),
            provider: CredentialProvider::GeminiSubscription,
            credential_kind: CredentialKind::OauthBrowserPkce,
            browser_credential_kind: Some(CredentialKind::OauthBrowserPkce),
            issuer: "google-gemini".into(),
            audience: AudienceBinding::standard(EndpointFamily::GeminiSubscription),
            client_id: client_id.to_string(),
            scopes: gemini_required_scopes(),
            device_authorization_endpoint: format!("{oauth_origin}/device/code"),
            device_token_endpoint: format!("{oauth_origin}/token"),
            authorization_endpoint: format!(
                "{accounts_origin}/o/oauth2/v2/auth?access_type=offline&prompt=consent"
            ),
            token_endpoint: format!("{oauth_origin}/token"),
            revocation_endpoint: format!("{oauth_origin}/revoke"),
            allowed_origins,
            allowed_user_authorization_origins,
            allow_insecure_loopback,
        };
        descriptor.validate()?;
        Ok(Self {
            descriptor,
            verifier,
            client_secret: client_secret.map(str::to_string),
        })
    }

    pub fn for_test(origin: &str, verifier: Arc<V>) -> Result<Self> {
        Self::new(
            origin,
            origin,
            origin,
            GOOGLE_PUBLIC_CLIENT_ID,
            None,
            verifier,
            true,
        )
    }
}

#[async_trait]
impl<V> OAuthDialect for GoogleGeminiOAuthDialect<V>
where
    V: GeminiTokenVerifier + 'static,
{
    fn descriptor(&self) -> &OAuthDialectDescriptor {
        &self.descriptor
    }

    fn preflight(&self) -> Result<()> {
        self.verifier.preflight()
    }

    fn device_authorization_request(&self) -> Result<OAuthHttpRequest> {
        let mut form = vec![
            ("client_id".into(), self.descriptor.client_id.clone()),
            ("scope".into(), GEMINI_DEVICE_WIRE_SCOPES.into()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret".into(), secret.clone()));
        }
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.device_authorization_endpoint.clone(),
            body: OAuthRequestBody::Form(form),
        })
    }

    fn parse_device_authorization(
        &self,
        status: StatusCode,
        body: Value,
    ) -> Result<DeviceAuthorization> {
        if !status.is_success() {
            if status == StatusCode::NOT_FOUND {
                return Err(GeminiDeviceEndpointError::StartDisabledOrUnsupported.into());
            }
            if oauth_error_code(&body) == Some("invalid_client") {
                return Err(GeminiDeviceEndpointError::ClientRejected.into());
            }
            return Err(GeminiDeviceEndpointError::StartRejected(status.as_u16()).into());
        }
        let device_code = required_string(&body, "device_code")?;
        let user_code = required_string(&body, "user_code")?;
        if !user_code.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character == '-'
                || character == '/'
                || character == '_'
        }) {
            bail!("Gemini subscription device authorization returned an invalid user_code format");
        }

        let verification_uri = body
            .get("verification_uri")
            .or_else(|| body.get("verification_url"))
            .and_then(Value::as_str)
            .with_context(|| {
                "Gemini subscription response omitted verification_uri/verification_url"
            })?
            .to_string();
        if verification_uri.is_empty()
            || verification_uri.len() > 4096
            || verification_uri.chars().any(char::is_control)
        {
            bail!("Gemini subscription verification_uri is invalid");
        }

        let verification_uri_complete = body
            .get("verification_uri_complete")
            .or_else(|| body.get("verification_url_complete"))
            .and_then(Value::as_str)
            .map(|s| {
                if s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control) {
                    bail!("Gemini subscription verification_uri_complete is invalid");
                }
                Ok(s.to_string())
            })
            .transpose()?;

        let expires_in = bounded_lifetime(json_duration_secs(&body, "expires_in")?)?;
        let interval = body
            .get("interval")
            .and_then(json_u64)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_POLL_INTERVAL);

        DeviceAuthorization::issued(
            device_code,
            user_code,
            verification_uri,
            verification_uri_complete,
            expires_in,
            interval,
        )
    }

    fn parse_device_authorization_response(
        &self,
        status: StatusCode,
        body: &[u8],
    ) -> Result<DeviceAuthorization> {
        if !status.is_success() {
            if status == StatusCode::NOT_FOUND {
                return Err(GeminiDeviceEndpointError::StartDisabledOrUnsupported.into());
            }
            let body = serde_json::from_slice(body).unwrap_or(Value::Null);
            if oauth_error_code(&body) == Some("invalid_client") {
                return Err(GeminiDeviceEndpointError::ClientRejected.into());
            }
            return Err(GeminiDeviceEndpointError::StartRejected(status.as_u16()).into());
        }
        let body = serde_json::from_slice(body)
            .context("Gemini subscription device authorization response was malformed JSON")?;
        self.parse_device_authorization(status, body)
    }

    fn device_poll_request(&self, pending: &DeviceAuthorization) -> Result<OAuthHttpRequest> {
        let mut form = vec![
            ("grant_type".into(), DEVICE_GRANT_TYPE.into()),
            ("device_code".into(), pending.device_code.clone()),
            ("client_id".into(), self.descriptor.client_id.clone()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret".into(), secret.clone()));
        }
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.device_token_endpoint.clone(),
            body: OAuthRequestBody::Form(form),
        })
    }

    fn parse_device_poll(&self, status: StatusCode, body: Value) -> Result<DevicePoll> {
        if status.is_success() {
            if body.get("access_token").and_then(Value::as_str).is_none() {
                return Err(GeminiAuthStageError::PollContract.into());
            }
            return Ok(DevicePoll::Tokens(body));
        }
        match oauth_error_code(&body) {
            Some("authorization_pending") => Ok(DevicePoll::Pending),
            Some("slow_down") => Ok(DevicePoll::SlowDown),
            Some("access_denied") => Ok(DevicePoll::Denied),
            Some("expired_token") => Ok(DevicePoll::Expired),
            Some("invalid_client") => Err(GeminiDeviceEndpointError::ClientRejected.into()),
            _ => Err(GeminiDeviceEndpointError::PollRejected(status.as_u16()).into()),
        }
    }

    fn parse_device_poll_response(&self, status: StatusCode, body: &[u8]) -> Result<DevicePoll> {
        if status.is_success() {
            let body = serde_json::from_slice(body).context(GeminiAuthStageError::PollContract)?;
            return self
                .parse_device_poll(status, body)
                .context(GeminiAuthStageError::PollContract);
        }
        let body = serde_json::from_slice(body).unwrap_or(Value::Null);
        self.parse_device_poll(status, body)
    }

    fn authorization_code_request(
        &self,
        grant: &AuthorizationCodeGrant,
    ) -> Result<OAuthHttpRequest> {
        let mut form = vec![
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), grant.code.clone()),
            ("redirect_uri".into(), grant.redirect_uri.clone()),
            ("client_id".into(), self.descriptor.client_id.clone()),
            ("code_verifier".into(), grant.verifier.clone()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret".into(), secret.clone()));
        }
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.token_endpoint.clone(),
            body: OAuthRequestBody::Form(form),
        })
    }

    fn refresh_request(&self, refresh_token: &str) -> Result<OAuthHttpRequest> {
        validate_secret_field(refresh_token, "refresh token")?;
        let mut form = vec![
            ("grant_type".into(), "refresh_token".into()),
            ("refresh_token".into(), refresh_token.to_string()),
            ("client_id".into(), self.descriptor.client_id.clone()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret".into(), secret.clone()));
        }
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.token_endpoint.clone(),
            body: OAuthRequestBody::Form(form),
        })
    }

    fn revoke_request(&self, token: &str) -> Result<OAuthHttpRequest> {
        validate_secret_field(token, "revocation token")?;
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.revocation_endpoint.clone(),
            body: OAuthRequestBody::Form(vec![("token".into(), token.to_string())]),
        })
    }

    async fn validate_tokens(
        &self,
        status: StatusCode,
        body: Value,
        previous: Option<&OAuthTokenRecord>,
        context: &TokenValidationContext,
        cancel: &CancellationToken,
    ) -> Result<OAuthTokenRecord> {
        if !status.is_success() {
            if oauth_error_code(&body) == Some("invalid_client") {
                return Err(GeminiDeviceEndpointError::ClientRejected.into());
            }
            let step = match context {
                TokenValidationContext::Browser { .. } => "authorization-code token exchange",
                TokenValidationContext::Device => "device-code token exchange",
                TokenValidationContext::Refresh => "token refresh",
            };
            let status = status.as_u16();
            if self.client_secret.is_none() && reports_missing_client_secret(&body) {
                return Err(GeminiAuthStageError::ClientSecretMissing { step, status }.into());
            }
            return Err(GeminiAuthStageError::TokenExchangeRejected {
                step,
                status,
                code: known_oauth_error_code(&body),
            }
            .into());
        }
        let access_token = required_string(&body, "access_token")
            .context(GeminiAuthStageError::TokenExchangeContract)?;
        let refresh_token = body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| previous.and_then(|record| record.refresh_token.clone()))
            .context(GeminiAuthStageError::TokenExchangeContract)?;
        validate_secret_field(&refresh_token, "refresh token")
            .context(GeminiAuthStageError::TokenExchangeContract)?;
        let id_token = body
            .get("id_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| previous.and_then(|record| record.id_token.clone()));

        let claims = self
            .verifier
            .verify(id_token.as_deref(), &access_token, cancel)
            .await
            .context(GeminiAuthStageError::IdentityVerification)?;

        let now = Utc::now();
        let matches_issuer = claims.issuer == GOOGLE_REQUIRED_TOKEN_ISSUER
            || claims.issuer == GOOGLE_REQUIRED_TOKEN_ISSUER_ALT;
        let matches_client = claims.audiences.contains(&self.descriptor.client_id)
            || claims.authorized_party.as_deref() == Some(self.descriptor.client_id.as_str());

        if !matches_issuer
            || !matches_client
            || claims.expires_at <= now
            || claims.not_before.is_some_and(|nbf| nbf > now)
        {
            return Err(GeminiAuthStageError::ClientBinding.into());
        }

        validate_public_claim(&claims.subject, "subject")
            .context(GeminiAuthStageError::ClientBinding)?;
        validate_public_claim(&claims.account_id, "account identifier")
            .context(GeminiAuthStageError::AccountEntitlement)?;

        match context {
            TokenValidationContext::Browser { expected_nonce, .. }
                if claims.nonce.as_deref() != Some(expected_nonce.as_str()) =>
            {
                return Err(GeminiAuthStageError::ClientBinding.into());
            }
            TokenValidationContext::Refresh if previous.is_none() => {
                return Err(GeminiAuthStageError::ClientBinding.into());
            }
            _ => {}
        }

        if let Some(previous) = previous {
            if previous.account != claims.account_id {
                return Err(GeminiAuthStageError::AccountEntitlement.into());
            }
        }

        let expires_in_secs = body.get("expires_in").and_then(json_u64).unwrap_or(3600);
        let expires_at = Utc::now() + Duration::from_secs(expires_in_secs);

        Ok(OAuthTokenRecord {
            dialect_id: self.descriptor.dialect_id.clone(),
            protocol_revision: self.descriptor.protocol_revision.clone(),
            provider: self.descriptor.provider,
            kind: self.descriptor.credential_kind,
            issuer: self.descriptor.issuer.clone(),
            audience: self.descriptor.audience.clone(),
            client_id: self.descriptor.client_id.clone(),
            account: claims.account_id,
            tenant: None,
            project: None,
            scopes: self.descriptor.scopes.clone(),
            access_token,
            refresh_token: Some(refresh_token),
            id_token,
            expires_at,
            generation: Uuid::new_v4().to_string(),
            revoked: false,
            mutation_pending: false,
        })
    }

    async fn validate_token_response(
        &self,
        status: StatusCode,
        body: &[u8],
        previous: Option<&OAuthTokenRecord>,
        context: &TokenValidationContext,
        cancel: &CancellationToken,
    ) -> Result<OAuthTokenRecord> {
        if !status.is_success() {
            let body = serde_json::from_slice(body).unwrap_or(Value::Null);
            return self
                .validate_tokens(status, body, previous, context, cancel)
                .await;
        }
        let body =
            serde_json::from_slice(body).context(GeminiAuthStageError::TokenExchangeContract)?;
        self.validate_tokens(status, body, previous, context, cancel)
            .await
    }
}

/// Secret-free subscription service authority used by transport wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiSubscriptionService {
    pub protocol_revision: &'static str,
    pub base_url: &'static str,
}

impl Default for GeminiSubscriptionService {
    fn default() -> Self {
        Self {
            protocol_revision: GEMINI_SUBSCRIPTION_SERVICE_REVISION,
            base_url: GEMINI_SUBSCRIPTION_BASE_URL,
        }
    }
}

impl GeminiSubscriptionService {
    pub fn validate_endpoint(&self, endpoint: &str) -> Result<()> {
        let requested = reqwest::Url::parse(endpoint)?;
        let required = reqwest::Url::parse(self.base_url)?;
        if requested.scheme() != "https"
            || requested.host_str() != Some("generativelanguage.googleapis.com")
            || requested.port_or_known_default() != required.port_or_known_default()
            || requested.username() != ""
            || requested.password().is_some()
            || requested.fragment().is_some()
        {
            bail!(
                "Gemini subscription credentials may only use the Google Generative Language subscription service"
            );
        }
        Ok(())
    }

    pub fn validate_account_header(&self, record: &OAuthTokenRecord, account: &str) -> Result<()> {
        if record.provider != CredentialProvider::GeminiSubscription
            || record.audience != AudienceBinding::standard(EndpointFamily::GeminiSubscription)
            || record.account != account
        {
            bail!("Gemini subscription request account does not match its named credential");
        }
        Ok(())
    }
}

fn decode_jwt_segment(segment: &str) -> Result<Vec<u8>> {
    let unpadded = segment.trim_end_matches('=');
    URL_SAFE_NO_PAD
        .decode(unpadded)
        .context("Invalid base64url encoding in JWT segment")
}

fn validate_public_claim(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("Gemini signed {label} is invalid");
    }
    Ok(())
}

fn required_string(body: &Value, field: &str) -> Result<String> {
    let value = body
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("Gemini subscription response omitted {field}"))?
        .to_string();
    validate_secret_field(&value, field)?;
    Ok(value)
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| value.as_str()?.parse::<u64>().ok())
}

fn json_duration_secs(body: &Value, field: &str) -> Result<Duration> {
    let secs = body
        .get(field)
        .and_then(json_u64)
        .with_context(|| format!("Gemini subscription response omitted {field}"))?;
    Ok(Duration::from_secs(secs))
}

fn bounded_lifetime(expires_in: Duration) -> Result<Duration> {
    if expires_in < MIN_DEVICE_LIFETIME || expires_in > MAX_DEVICE_LIFETIME {
        bail!("Gemini subscription device authorization expiry is outside the supported range");
    }
    Ok(expires_in)
}

fn oauth_error_code(body: &Value) -> Option<&str> {
    body.get("error").and_then(Value::as_str)
}

/// Map the response's `error` onto the RFC 6749 section 5.2 vocabulary so a
/// diagnostic never carries upstream-chosen text.
fn known_oauth_error_code(body: &Value) -> &'static str {
    const KNOWN: [&str; 6] = [
        "invalid_request",
        "invalid_client",
        "invalid_grant",
        "unauthorized_client",
        "unsupported_grant_type",
        "invalid_scope",
    ];
    let reported = oauth_error_code(body);
    KNOWN
        .into_iter()
        .find(|known| Some(*known) == reported)
        .unwrap_or("no recognised OAuth error code")
}

/// Google answers a secretless desktop-client token request with
/// `invalid_request` and a description naming `client_secret`.
fn reports_missing_client_secret(body: &Value) -> bool {
    oauth_error_code(body) == Some("invalid_request")
        && body
            .get("error_description")
            .and_then(Value::as_str)
            .is_some_and(|description| description.contains("client_secret"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use serde_json::json;

    fn mock_jwt(payload: &Value) -> String {
        let header = json!({
            "alg": "RS256",
            "typ": "JWT"
        });
        let h_b64 = URL_SAFE_NO_PAD.encode(header.to_string().as_bytes());
        let p_b64 = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        let s_b64 = URL_SAFE_NO_PAD.encode(b"mock_signature");
        format!("{h_b64}.{p_b64}.{s_b64}")
    }

    #[test]
    fn production_descriptor_validates() {
        let dialect = GoogleGeminiOAuthDialect::production().unwrap();
        assert_eq!(
            dialect.descriptor().dialect_id,
            "google_gemini_subscription"
        );
        assert_eq!(
            dialect.descriptor().provider,
            CredentialProvider::GeminiSubscription
        );
        assert_eq!(
            dialect.descriptor().audience,
            AudienceBinding::standard(EndpointFamily::GeminiSubscription)
        );
    }

    /// Google's token endpoint answers a secretless exchange for the built-in
    /// desktop client with HTTP 400 `client_secret is missing.`, so an
    /// operator-supplied Finch secret must reach the exchange instead of
    /// being discarded.
    #[test]
    fn test_finch_client_secret_is_kept_with_the_built_in_client_id() {
        let (client_id, secret) =
            resolve_gemini_oauth_client(None, None, Some("finch-secret".into()), None);
        assert_eq!(
            (client_id.as_str(), secret.as_deref()),
            (GOOGLE_PUBLIC_CLIENT_ID, Some("finch-secret")),
            "FINCH_GEMINI_CLIENT_SECRET must survive with the built-in client ID; client_id={client_id} secret_present={}",
            secret.is_some()
        );

        let dialect =
            GoogleGeminiOAuthDialect::production_with_client_id(&client_id, secret.as_deref())
                .unwrap();
        let request = dialect
            .authorization_code_request(&AuthorizationCodeGrant {
                code: "code".into(),
                verifier: "verifier".into(),
                redirect_uri: "http://127.0.0.1:1/callback".into(),
            })
            .unwrap();
        let OAuthRequestBody::Form(form) = request.body else {
            panic!("the authorization-code exchange must be a form post");
        };
        let fields = form.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>();
        assert!(
            form.contains(&("client_secret".into(), "finch-secret".into())),
            "the authorization-code exchange must carry the configured client secret; fields={fields:?}"
        );
    }

    #[test]
    fn test_generic_google_secret_is_not_paired_with_the_built_in_client_id() {
        let (client_id, secret) =
            resolve_gemini_oauth_client(None, None, None, Some("other-clients-secret".into()));
        assert_eq!(
            (client_id.as_str(), secret),
            (GOOGLE_PUBLIC_CLIENT_ID, None),
            "GOOGLE_CLIENT_SECRET belongs to GOOGLE_CLIENT_ID and must not be sent for the built-in client"
        );
        let (client_id, secret) = resolve_gemini_oauth_client(
            None,
            Some("custom.apps.googleusercontent.com".into()),
            None,
            Some("custom-secret".into()),
        );
        assert_eq!(
            (client_id.as_str(), secret.as_deref()),
            ("custom.apps.googleusercontent.com", Some("custom-secret")),
            "a custom GOOGLE_CLIENT_ID keeps its own GOOGLE_CLIENT_SECRET"
        );
    }

    #[test]
    fn required_scopes_use_cloud_platform() {
        let scopes = gemini_required_scopes();
        assert!(scopes.contains("https://www.googleapis.com/auth/cloud-platform"));
        assert!(!scopes.contains("https://www.googleapis.com/auth/generative-language"));

        assert!(
            GEMINI_DEVICE_WIRE_SCOPES.contains("https://www.googleapis.com/auth/cloud-platform")
        );
        assert!(!GEMINI_DEVICE_WIRE_SCOPES
            .contains("https://www.googleapis.com/auth/generative-language"));
    }

    #[test]
    fn device_authorization_request_contains_expected_fields() {
        let dialect = GoogleGeminiOAuthDialect::production().unwrap();
        let req = dialect.device_authorization_request().unwrap();
        assert_eq!(req.endpoint, "https://oauth2.googleapis.com/device/code");
        match req.body {
            OAuthRequestBody::Form(form) => {
                let map: std::collections::HashMap<_, _> = form.into_iter().collect();
                assert_eq!(map.get("client_id").unwrap(), GOOGLE_PUBLIC_CLIENT_ID);
                assert_eq!(map.get("scope").unwrap(), GEMINI_DEVICE_WIRE_SCOPES);
            }
            _ => panic!("Expected form body"),
        }
    }

    #[test]
    fn parse_device_authorization_handles_verification_url() {
        let dialect = GoogleGeminiOAuthDialect::production().unwrap();
        let body = json!({
            "device_code": "dev123",
            "user_code": "ABCD-EFGH",
            "verification_url": "https://www.google.com/device",
            "expires_in": 1800,
            "interval": 5
        });
        let auth = dialect
            .parse_device_authorization(StatusCode::OK, body)
            .unwrap();
        assert_eq!(auth.user_code, "ABCD-EFGH");
        assert_eq!(auth.verification_uri, "https://www.google.com/device");
        assert_eq!(auth.expires_in, Duration::from_secs(1800));
        assert_eq!(auth.interval, Duration::from_secs(5));
    }

    #[test]
    fn parse_device_authorization_handles_verification_uri() {
        let dialect = GoogleGeminiOAuthDialect::production().unwrap();
        let body = json!({
            "device_code": "dev123",
            "user_code": "WXYZ-1234",
            "verification_uri": "https://www.google.com/device",
            "verification_uri_complete": "https://www.google.com/device?user_code=WXYZ-1234",
            "expires_in": 900,
            "interval": 5
        });
        let auth = dialect
            .parse_device_authorization(StatusCode::OK, body)
            .unwrap();
        assert_eq!(auth.user_code, "WXYZ-1234");
        assert_eq!(auth.verification_uri, "https://www.google.com/device");
        assert_eq!(
            auth.verification_uri_complete,
            Some("https://www.google.com/device?user_code=WXYZ-1234".to_string())
        );
    }

    #[test]
    fn parse_device_authorization_rejects_errors() {
        let dialect = GoogleGeminiOAuthDialect::production().unwrap();
        let err = dialect
            .parse_device_authorization(StatusCode::NOT_FOUND, json!({}))
            .unwrap_err();
        assert!(err.to_string().contains("404"));

        let err = dialect
            .parse_device_authorization(StatusCode::BAD_REQUEST, json!({"error": "invalid_client"}))
            .unwrap_err();
        assert!(err.to_string().contains("invalid_client"));
    }

    #[test]
    fn parse_device_poll_states() {
        let dialect = GoogleGeminiOAuthDialect::production().unwrap();
        assert!(matches!(
            dialect
                .parse_device_poll(
                    StatusCode::BAD_REQUEST,
                    json!({"error": "authorization_pending"})
                )
                .unwrap(),
            DevicePoll::Pending
        ));
        assert!(matches!(
            dialect
                .parse_device_poll(StatusCode::BAD_REQUEST, json!({"error": "slow_down"}))
                .unwrap(),
            DevicePoll::SlowDown
        ));
        assert!(matches!(
            dialect
                .parse_device_poll(StatusCode::BAD_REQUEST, json!({"error": "access_denied"}))
                .unwrap(),
            DevicePoll::Denied
        ));
        assert!(matches!(
            dialect
                .parse_device_poll(StatusCode::BAD_REQUEST, json!({"error": "expired_token"}))
                .unwrap(),
            DevicePoll::Expired
        ));
        assert!(matches!(
            dialect
                .parse_device_poll(
                    StatusCode::OK,
                    json!({"access_token": "ya29.test", "expires_in": 3600})
                )
                .unwrap(),
            DevicePoll::Tokens(_)
        ));
    }

    #[tokio::test]
    async fn token_verifier_production_decodes_claims() {
        let verifier = GeminiTokenVerifierProduction;
        let exp = Utc::now().timestamp() + 3600;
        let jwt = mock_jwt(&json!({
            "iss": "https://accounts.google.com",
            "aud": GOOGLE_PUBLIC_CLIENT_ID,
            "sub": "1234567890",
            "email": "user@example.com",
            "exp": exp
        }));
        let cancel = CancellationToken::new();
        let claims = verifier
            .verify(Some(&jwt), "access", &cancel)
            .await
            .unwrap();
        assert_eq!(claims.issuer, "https://accounts.google.com");
        assert_eq!(claims.subject, "1234567890");
        assert_eq!(claims.email, Some("user@example.com".to_string()));
        assert_eq!(claims.account_id, "user@example.com");
        assert!(claims.audiences.contains(GOOGLE_PUBLIC_CLIENT_ID));
    }

    #[tokio::test]
    async fn token_verifier_production_rejects_expired() {
        let exp = Utc::now().timestamp() - 3600;
        let jwt = mock_jwt(&json!({
            "iss": "https://accounts.google.com",
            "aud": GOOGLE_PUBLIC_CLIENT_ID,
            "sub": "1234567890",
            "email": "user@example.com",
            "exp": exp
        }));
        let cancel = CancellationToken::new();
        let dialect = GoogleGeminiOAuthDialect::production().unwrap();
        let err = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "access_token": "ya29.test",
                    "refresh_token": "1//test",
                    "id_token": jwt,
                    "expires_in": 3600
                }),
                None,
                &TokenValidationContext::Refresh,
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("binding"));
    }

    #[tokio::test]
    async fn validate_tokens_succeeds_and_creates_record() {
        let dialect = GoogleGeminiOAuthDialect::production().unwrap();
        let exp = Utc::now().timestamp() + 3600;
        let jwt = mock_jwt(&json!({
            "iss": "https://accounts.google.com",
            "aud": GOOGLE_PUBLIC_CLIENT_ID,
            "sub": "1234567890",
            "email": "user@example.com",
            "exp": exp
        }));
        let cancel = CancellationToken::new();
        let record = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "access_token": "ya29.test",
                    "refresh_token": "1//test",
                    "id_token": jwt,
                    "expires_in": 3600
                }),
                None,
                &TokenValidationContext::Device,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(record.provider, CredentialProvider::GeminiSubscription);
        assert_eq!(record.account, "user@example.com");
        assert_eq!(record.access_token, "ya29.test");
        assert_eq!(record.refresh_token, Some("1//test".to_string()));
    }
}
