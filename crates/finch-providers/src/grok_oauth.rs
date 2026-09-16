//! xAI SuperGrok / Grok subscription OAuth compatibility dialect.
//!
//! This adapter reproduces the public-client protocol visible in the
//! open-source grok-build CLI and in xAI's OIDC discovery document. It is not
//! the xAI Console API. Tokens from it must never be sent to `api.x.ai`.
//! The compatibility revision is a fail-closed fence, not a claim of a stable
//! third-party contract.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
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

pub const GROK_OAUTH_PROTOCOL_REVISION: &str =
    "xai-grok-build-public-client@482711333c7195dc16a272777f86086d615e2afb+finch-binding-v1";
pub const GROK_SUBSCRIPTION_SERVICE_REVISION: &str =
    "xai-cli-chat-proxy@482711333c7195dc16a272777f86086d615e2afb";
pub const XAI_PUBLIC_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub(crate) const XAI_AUTH_ORIGIN: &str = "https://auth.x.ai";
pub(crate) const XAI_ACCOUNTS_ORIGIN: &str = "https://accounts.x.ai";
pub const GROK_SUBSCRIPTION_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
pub const GROK_REQUIRED_TOKEN_ISSUER: &str = "https://auth.x.ai";
pub const GROK_SESSION_TOKEN_HEADER: &str = "xai-grok-cli";
const GROK_OAUTH_REFERRER: &str = "finch";
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAX_DEVICE_LIFETIME: Duration = Duration::from_secs(15 * 60);
const MIN_DEVICE_LIFETIME: Duration = Duration::from_secs(1);
const GROK_DEVICE_WIRE_SCOPES: &str = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write workspaces:read workspaces:write";

/// Status-only xAI device endpoint failures. Upstream bodies are never
/// retained in these typed causes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrokDeviceEndpointError {
    #[error(
        "Grok subscription device authorization is disabled or unsupported for this account (HTTP 404). Use an xAI API key from console.x.ai instead"
    )]
    StartDisabledOrUnsupported,
    #[error("Grok subscription device authorization was rejected by xAI (invalid_client)")]
    ClientRejected,
    #[error("Grok subscription device authorization is unavailable (HTTP {0})")]
    StartRejected(u16),
    #[error("Grok subscription device polling ended (HTTP {0})")]
    PollRejected(u16),
}

/// Secret-free stage markers for actionable device-login diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrokAuthStageError {
    #[error("Grok subscription device polling response changed after browser authorization")]
    PollContract,
    #[error("Grok subscription token exchange was rejected (HTTP {0})")]
    TokenExchangeRejected(u16),
    #[error("Grok subscription token exchange response changed")]
    TokenExchangeContract,
    #[error("Grok subscription signed identity verification failed")]
    IdentityVerification,
    #[error("Grok subscription signed client binding failed")]
    ClientBinding,
    #[error("Grok subscription signed account entitlement is missing or invalid")]
    AccountEntitlement,
}

/// Finch-local capability attached to a verified Grok subscription credential.
///
/// Wire OAuth scopes are the frozen grok-build set. Token response `scope`
/// fields are not proof of requested authority and are not projected into
/// Finch's credential graph.
pub fn grok_required_scopes() -> BTreeSet<String> {
    BTreeSet::from(["grok-cli:access".into()])
}

/// Signature-verified provider claims. The adapter does not parse an
/// unverified JWT payload and call it identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGrokClaims {
    pub issuer: String,
    pub audiences: BTreeSet<String>,
    pub authorized_party: Option<String>,
    pub subject: String,
    pub account_id: String,
    pub principal_type: Option<String>,
    pub nonce: Option<String>,
    pub expires_at: chrono::DateTime<Utc>,
    pub not_before: Option<chrono::DateTime<Utc>>,
}

/// Injected JWS/JWKS verification boundary. Production enablement must supply
/// an implementation pinned to the dialect's exact issuer and algorithms.
#[async_trait]
pub trait GrokTokenVerifier: Send + Sync {
    fn preflight(&self) -> Result<()>;
    async fn verify(
        &self,
        id_token: Option<&str>,
        access_token: &str,
        cancel: &CancellationToken,
    ) -> Result<VerifiedGrokClaims>;
}

/// Fail-closed default until Finch's audited xAI JWKS verifier is wired.
#[derive(Debug, Default)]
pub struct GrokVerificationUnavailable;

#[async_trait]
impl GrokTokenVerifier for GrokVerificationUnavailable {
    fn preflight(&self) -> Result<()> {
        unavailable_verifier_error()
    }

    async fn verify(
        &self,
        _id_token: Option<&str>,
        _access_token: &str,
        _cancel: &CancellationToken,
    ) -> Result<VerifiedGrokClaims> {
        unavailable_verifier_error()
    }
}

fn unavailable_verifier_error<T>() -> Result<T> {
    bail!("Grok subscription OAuth token verification is unavailable for this compatibility revision; update Finch rather than bypassing issuer or signature validation")
}

/// Strict xAI-specific dialect; reusable OAuth state remains in `oauth`.
pub struct XaiGrokOAuthDialect<V> {
    descriptor: OAuthDialectDescriptor,
    verifier: Arc<V>,
}

#[cfg(feature = "grok_subscription")]
impl XaiGrokOAuthDialect<crate::grok_jwks::GrokJwksVerifier> {
    pub fn production() -> Result<Self> {
        Self::new(
            XAI_AUTH_ORIGIN,
            XAI_ACCOUNTS_ORIGIN,
            Arc::new(crate::grok_jwks::GrokJwksVerifier::production()?),
            false,
        )
    }
}

impl<V> XaiGrokOAuthDialect<V>
where
    V: GrokTokenVerifier,
{
    fn new(
        auth_origin: &str,
        accounts_origin: &str,
        verifier: Arc<V>,
        allow_insecure_loopback: bool,
    ) -> Result<Self> {
        let auth_origin = auth_origin.trim_end_matches('/').to_string();
        let accounts_origin = accounts_origin.trim_end_matches('/').to_string();
        let allowed_origins = BTreeSet::from([auth_origin.clone()]);
        let descriptor = OAuthDialectDescriptor {
            dialect_id: "xai_grok_subscription".into(),
            protocol_revision: GROK_OAUTH_PROTOCOL_REVISION.into(),
            provider: CredentialProvider::GrokSubscription,
            credential_kind: CredentialKind::OauthDevice,
            browser_credential_kind: None,
            issuer: "xai-grok".into(),
            audience: AudienceBinding::standard(EndpointFamily::GrokSubscription),
            client_id: XAI_PUBLIC_CLIENT_ID.into(),
            scopes: grok_required_scopes(),
            device_authorization_endpoint: format!("{auth_origin}/oauth2/device/code"),
            device_token_endpoint: format!("{auth_origin}/oauth2/token"),
            authorization_endpoint: format!("{auth_origin}/oauth2/authorize"),
            token_endpoint: format!("{auth_origin}/oauth2/token"),
            revocation_endpoint: format!("{auth_origin}/oauth2/revoke"),
            allowed_origins,
            allowed_user_authorization_origins: BTreeSet::from([accounts_origin]),
            allow_insecure_loopback,
        };
        descriptor.validate()?;
        Ok(Self {
            descriptor,
            verifier,
        })
    }

    pub fn for_test(auth_origin: &str, verifier: Arc<V>) -> Result<Self> {
        Self::new(auth_origin, auth_origin, verifier, true)
    }
}

#[async_trait]
impl<V> OAuthDialect for XaiGrokOAuthDialect<V>
where
    V: GrokTokenVerifier + 'static,
{
    fn descriptor(&self) -> &OAuthDialectDescriptor {
        &self.descriptor
    }

    fn preflight(&self) -> Result<()> {
        self.verifier.preflight()
    }

    fn device_authorization_request(&self) -> Result<OAuthHttpRequest> {
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.device_authorization_endpoint.clone(),
            body: OAuthRequestBody::Form(vec![
                ("client_id".into(), self.descriptor.client_id.clone()),
                ("scope".into(), GROK_DEVICE_WIRE_SCOPES.into()),
                ("referrer".into(), GROK_OAUTH_REFERRER.into()),
            ]),
        })
    }

    fn parse_device_authorization(
        &self,
        status: StatusCode,
        body: Value,
    ) -> Result<DeviceAuthorization> {
        if !status.is_success() {
            if status == StatusCode::NOT_FOUND {
                return Err(GrokDeviceEndpointError::StartDisabledOrUnsupported.into());
            }
            if oauth_error_code(&body) == Some("invalid_client") {
                return Err(GrokDeviceEndpointError::ClientRejected.into());
            }
            return Err(GrokDeviceEndpointError::StartRejected(status.as_u16()).into());
        }
        let device_code = required_string(&body, "device_code")?;
        let user_code = required_string(&body, "user_code")?;
        if !user_code
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            bail!("Grok subscription device authorization returned an invalid user_code format");
        }
        let verification_uri = required_public_uri(&body, "verification_uri")?;
        let verification_uri_complete = optional_public_uri(&body, "verification_uri_complete")?;
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
                return Err(GrokDeviceEndpointError::StartDisabledOrUnsupported.into());
            }
            let body = serde_json::from_slice(body).unwrap_or(Value::Null);
            if oauth_error_code(&body) == Some("invalid_client") {
                return Err(GrokDeviceEndpointError::ClientRejected.into());
            }
            return Err(GrokDeviceEndpointError::StartRejected(status.as_u16()).into());
        }
        let body = serde_json::from_slice(body)
            .context("Grok subscription device authorization response was malformed JSON")?;
        self.parse_device_authorization(status, body)
    }

    fn device_poll_request(&self, pending: &DeviceAuthorization) -> Result<OAuthHttpRequest> {
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.device_token_endpoint.clone(),
            body: OAuthRequestBody::Form(vec![
                ("grant_type".into(), DEVICE_GRANT_TYPE.into()),
                ("device_code".into(), pending.device_code.clone()),
                ("client_id".into(), self.descriptor.client_id.clone()),
            ]),
        })
    }

    fn parse_device_poll(&self, status: StatusCode, body: Value) -> Result<DevicePoll> {
        if status.is_success() {
            if body.get("access_token").and_then(Value::as_str).is_none() {
                return Err(GrokAuthStageError::PollContract.into());
            }
            return Ok(DevicePoll::Tokens(body));
        }
        match oauth_error_code(&body) {
            Some("authorization_pending") => Ok(DevicePoll::Pending),
            Some("slow_down") => Ok(DevicePoll::SlowDown),
            Some("access_denied") => Ok(DevicePoll::Denied),
            Some("expired_token") => Ok(DevicePoll::Expired),
            Some("invalid_client") => Err(GrokDeviceEndpointError::ClientRejected.into()),
            _ => Err(GrokDeviceEndpointError::PollRejected(status.as_u16()).into()),
        }
    }

    fn parse_device_poll_response(&self, status: StatusCode, body: &[u8]) -> Result<DevicePoll> {
        if status.is_success() {
            let body = serde_json::from_slice(body).context(GrokAuthStageError::PollContract)?;
            return self
                .parse_device_poll(status, body)
                .context(GrokAuthStageError::PollContract);
        }
        let body = serde_json::from_slice(body).unwrap_or(Value::Null);
        self.parse_device_poll(status, body)
    }

    fn authorization_code_request(
        &self,
        grant: &AuthorizationCodeGrant,
    ) -> Result<OAuthHttpRequest> {
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.token_endpoint.clone(),
            body: OAuthRequestBody::Form(vec![
                ("grant_type".into(), "authorization_code".into()),
                ("code".into(), grant.code.clone()),
                ("redirect_uri".into(), grant.redirect_uri.clone()),
                ("client_id".into(), self.descriptor.client_id.clone()),
                ("code_verifier".into(), grant.verifier.clone()),
            ]),
        })
    }

    fn refresh_request(&self, refresh_token: &str) -> Result<OAuthHttpRequest> {
        validate_secret_field(refresh_token, "refresh token")?;
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.token_endpoint.clone(),
            body: OAuthRequestBody::Form(vec![
                ("grant_type".into(), "refresh_token".into()),
                ("refresh_token".into(), refresh_token.to_string()),
                ("client_id".into(), self.descriptor.client_id.clone()),
            ]),
        })
    }

    fn revoke_request(&self, token: &str) -> Result<OAuthHttpRequest> {
        validate_secret_field(token, "revocation token")?;
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.revocation_endpoint.clone(),
            body: OAuthRequestBody::Form(vec![
                ("token".into(), token.to_string()),
                ("token_type_hint".into(), "refresh_token".into()),
                ("client_id".into(), self.descriptor.client_id.clone()),
            ]),
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
                return Err(GrokDeviceEndpointError::ClientRejected.into());
            }
            return Err(GrokAuthStageError::TokenExchangeRejected(status.as_u16()).into());
        }
        let access_token = required_string(&body, "access_token")
            .context(GrokAuthStageError::TokenExchangeContract)?;
        let refresh_token = body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| previous.and_then(|record| record.refresh_token.clone()))
            .context(GrokAuthStageError::TokenExchangeContract)?;
        validate_secret_field(&refresh_token, "refresh token")
            .context(GrokAuthStageError::TokenExchangeContract)?;
        let id_token = body
            .get("id_token")
            .and_then(Value::as_str)
            .map(str::to_string);
        let claims = self
            .verifier
            .verify(id_token.as_deref(), &access_token, cancel)
            .await
            .context(GrokAuthStageError::IdentityVerification)?;
        let now = Utc::now();
        if claims.issuer != GROK_REQUIRED_TOKEN_ISSUER
            || !claims.audiences.contains(&self.descriptor.client_id)
            || (claims.audiences.len() > 1
                && claims.authorized_party.as_deref() != Some(self.descriptor.client_id.as_str()))
            || claims
                .authorized_party
                .as_deref()
                .is_some_and(|party| party != self.descriptor.client_id)
            || claims.expires_at <= now
            || claims.not_before.is_some_and(|not_before| not_before > now)
        {
            return Err(GrokAuthStageError::ClientBinding.into());
        }
        validate_public_claim(&claims.subject, "subject")
            .context(GrokAuthStageError::ClientBinding)?;
        validate_public_claim(&claims.account_id, "account identifier")
            .context(GrokAuthStageError::AccountEntitlement)?;
        if let Some(principal_type) = claims.principal_type.as_deref() {
            validate_public_claim(principal_type, "principal type")
                .context(GrokAuthStageError::AccountEntitlement)?;
        }
        match context {
            TokenValidationContext::Browser { expected_nonce, .. }
                if claims.nonce.as_deref() != Some(expected_nonce.as_str()) =>
            {
                return Err(GrokAuthStageError::ClientBinding.into())
            }
            TokenValidationContext::Refresh if previous.is_none() => {
                return Err(GrokAuthStageError::ClientBinding.into())
            }
            _ => {}
        }
        if let Some(previous) = previous {
            if previous.account != claims.account_id {
                return Err(GrokAuthStageError::AccountEntitlement.into());
            }
        }
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
            expires_at: claims.expires_at,
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
            serde_json::from_slice(body).context(GrokAuthStageError::TokenExchangeContract)?;
        self.validate_tokens(status, body, previous, context, cancel)
            .await
    }
}

/// Secret-free subscription service authority used by transport wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokSubscriptionService {
    pub protocol_revision: &'static str,
    pub base_url: &'static str,
}

impl Default for GrokSubscriptionService {
    fn default() -> Self {
        Self {
            protocol_revision: GROK_SUBSCRIPTION_SERVICE_REVISION,
            base_url: GROK_SUBSCRIPTION_BASE_URL,
        }
    }
}

impl GrokSubscriptionService {
    /// Reject Console API and custom origin substitution before a session
    /// token is available to the transport.
    pub fn validate_endpoint(&self, endpoint: &str) -> Result<()> {
        let requested = reqwest::Url::parse(endpoint)?;
        let required = reqwest::Url::parse(self.base_url)?;
        let required_path = required.path().trim_end_matches('/');
        let path_matches = requested.path() == required_path
            || requested
                .path()
                .strip_prefix(required_path)
                .is_some_and(|suffix| suffix.starts_with('/'))
            || requested.path() == "/"
            || requested.path().is_empty();
        if requested.scheme() != "https"
            || requested.host_str() != Some("cli-chat-proxy.grok.com")
            || requested.port_or_known_default() != required.port_or_known_default()
            || !path_matches
            || requested.username() != ""
            || requested.password().is_some()
            || requested.fragment().is_some()
        {
            bail!("Grok subscription credentials may only use the versioned cli-chat-proxy.grok.com subscription service; xAI Console API and custom endpoints require distinct credentials");
        }
        Ok(())
    }

    pub fn validate_account_header(&self, record: &OAuthTokenRecord, account: &str) -> Result<()> {
        if record.provider != CredentialProvider::GrokSubscription
            || record.audience != AudienceBinding::standard(EndpointFamily::GrokSubscription)
            || record.account != account
        {
            bail!("Grok subscription request account does not match its named credential");
        }
        Ok(())
    }
}

fn validate_public_claim(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("Grok signed {label} is invalid");
    }
    Ok(())
}

fn required_string(body: &Value, field: &str) -> Result<String> {
    let value = body
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("Grok subscription response omitted {field}"))?
        .to_string();
    validate_secret_field(&value, field)?;
    Ok(value)
}

fn required_public_uri(body: &Value, field: &str) -> Result<String> {
    let value = body
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("Grok subscription response omitted {field}"))?
        .to_string();
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        bail!("Grok subscription {field} is invalid");
    }
    Ok(value)
}

fn optional_public_uri(body: &Value, field: &str) -> Result<Option<String>> {
    match body.get(field).and_then(Value::as_str) {
        None => Ok(None),
        Some(value) => {
            if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
                bail!("Grok subscription {field} is invalid");
            }
            Ok(Some(value.to_string()))
        }
    }
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
        .with_context(|| format!("Grok subscription response omitted {field}"))?;
    Ok(Duration::from_secs(secs))
}

fn bounded_lifetime(expires_in: Duration) -> Result<Duration> {
    if expires_in < MIN_DEVICE_LIFETIME || expires_in > MAX_DEVICE_LIFETIME {
        bail!("Grok subscription device authorization expiry is outside the supported range");
    }
    Ok(expires_in)
}

fn oauth_error_code(body: &Value) -> Option<&str> {
    body.get("error").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::OAuthCredentialStore;
    use anyhow::bail;
    use chrono::TimeDelta;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct NoopStore;

    impl OAuthCredentialStore for NoopStore {
        fn load(&self, _reference: &str) -> Result<Option<OAuthTokenRecord>> {
            Ok(None)
        }

        fn compare_and_swap(
            &self,
            _reference: &str,
            _expected_generation: Option<&str>,
            _replacement: &OAuthTokenRecord,
        ) -> Result<()> {
            bail!("unexpected credential persistence")
        }
    }

    #[derive(Clone)]
    struct FixedVerifier(VerifiedGrokClaims);

    #[async_trait]
    impl GrokTokenVerifier for FixedVerifier {
        fn preflight(&self) -> Result<()> {
            Ok(())
        }

        async fn verify(
            &self,
            _id_token: Option<&str>,
            _access_token: &str,
            _cancel: &CancellationToken,
        ) -> Result<VerifiedGrokClaims> {
            Ok(self.0.clone())
        }
    }

    fn claims() -> VerifiedGrokClaims {
        VerifiedGrokClaims {
            issuer: GROK_REQUIRED_TOKEN_ISSUER.into(),
            audiences: BTreeSet::from([XAI_PUBLIC_CLIENT_ID.into()]),
            authorized_party: None,
            subject: "subject-work".into(),
            account_id: "acct-work".into(),
            principal_type: None,
            nonce: None,
            expires_at: Utc::now() + TimeDelta::hours(1),
            not_before: None,
        }
    }

    fn record() -> OAuthTokenRecord {
        OAuthTokenRecord {
            dialect_id: "xai_grok_subscription".into(),
            protocol_revision: GROK_OAUTH_PROTOCOL_REVISION.into(),
            provider: CredentialProvider::GrokSubscription,
            kind: CredentialKind::OauthDevice,
            issuer: "xai-grok".into(),
            audience: AudienceBinding::standard(EndpointFamily::GrokSubscription),
            client_id: XAI_PUBLIC_CLIENT_ID.into(),
            account: "acct-work".into(),
            tenant: None,
            project: None,
            scopes: grok_required_scopes(),
            access_token: "subscription-bearer".into(),
            refresh_token: Some("subscription-refresh".into()),
            id_token: None,
            expires_at: Utc::now() + TimeDelta::hours(1),
            generation: "generation".into(),
            revoked: false,
            mutation_pending: false,
        }
    }

    #[test]
    fn subscription_service_never_crosses_to_console_api_or_silent_api_key_fallback() {
        let service = GrokSubscriptionService::default();
        service
            .validate_endpoint("https://cli-chat-proxy.grok.com/v1/chat/completions")
            .unwrap();
        service
            .validate_endpoint("https://cli-chat-proxy.grok.com/v1/models")
            .unwrap();
        for hostile in [
            "https://api.x.ai/v1/chat/completions",
            "https://cli-chat-proxy.grok.com.evil.example/v1/models",
            "https://cli-chat-proxy.grok.com/v1evil/models",
            "https://accounts.x.ai/v1/models",
        ] {
            assert!(service.validate_endpoint(hostile).is_err(), "{hostile}");
        }
        service
            .validate_account_header(&record(), "acct-work")
            .unwrap();
        assert!(service
            .validate_account_header(&record(), "acct-other")
            .is_err());
        let mut platform = record();
        platform.provider = CredentialProvider::Xai;
        assert!(service
            .validate_account_header(&platform, "acct-work")
            .is_err());
    }

    #[tokio::test]
    async fn unavailable_production_verifier_fails_client_preflight_before_any_socket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let dialect = XaiGrokOAuthDialect::new(
            &origin,
            &origin,
            Arc::new(GrokVerificationUnavailable),
            true,
        )
        .unwrap();
        let error = crate::oauth::OAuthClient::new(Arc::new(dialect), Arc::new(NoopStore))
            .unwrap_err()
            .to_string();
        assert!(error.contains("verification is unavailable"));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn public_client_risk_is_versioned_and_verification_is_exactly_pinned() {
        let dialect = XaiGrokOAuthDialect::production().unwrap();
        assert!(dialect
            .descriptor()
            .protocol_revision
            .contains("482711333c71"));
        assert!(!dialect
            .descriptor()
            .allowed_origins
            .contains("https://api.x.ai"));
        assert_eq!(
            dialect.descriptor().authorization_endpoint,
            "https://auth.x.ai/oauth2/authorize"
        );
        assert_eq!(
            dialect.descriptor().device_authorization_endpoint,
            "https://auth.x.ai/oauth2/device/code"
        );
        assert!(dialect
            .descriptor()
            .allowed_user_authorization_origins
            .contains("https://accounts.x.ai"));
        assert!(dialect.descriptor().browser_credential_kind.is_none());
        assert_eq!(dialect.descriptor().scopes, grok_required_scopes());
        assert!(dialect.preflight().is_ok());
    }

    #[tokio::test]
    async fn xai_dialect_exact_device_form_and_token_fixture_is_strictly_bound() {
        let dialect = XaiGrokOAuthDialect::for_test(
            "http://127.0.0.1:12345",
            Arc::new(FixedVerifier(claims())),
        )
        .unwrap();
        let request = dialect.device_authorization_request().unwrap();
        assert_eq!(
            request.endpoint,
            "http://127.0.0.1:12345/oauth2/device/code"
        );
        assert_eq!(
            request.body,
            OAuthRequestBody::Form(vec![
                ("client_id".into(), XAI_PUBLIC_CLIENT_ID.into()),
                ("scope".into(), GROK_DEVICE_WIRE_SCOPES.into()),
                ("referrer".into(), "finch".into()),
            ])
        );
        let pending = dialect
            .parse_device_authorization(
                StatusCode::OK,
                json!({
                    "device_code": "device-secret",
                    "user_code": "ABCD-EFGH",
                    "verification_uri": "http://127.0.0.1:12345/device",
                    "expires_in": 600,
                    "interval": 1
                }),
            )
            .unwrap();
        assert_eq!(pending.user_code, "ABCD-EFGH");
        let poll_request = dialect.device_poll_request(&pending).unwrap();
        assert_eq!(poll_request.endpoint, "http://127.0.0.1:12345/oauth2/token");
        let poll = dialect
            .parse_device_poll(
                StatusCode::OK,
                json!({
                    "access_token": "access-secret",
                    "refresh_token": "refresh-secret",
                    "expires_in": 3600
                }),
            )
            .unwrap();
        assert!(matches!(poll, DevicePoll::Tokens(_)));
        let tokens = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "access_token": "access-secret",
                    "refresh_token": "refresh-secret",
                    "id_token": "id-secret",
                    "expires_in": 3600
                }),
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let metadata = tokens.provider_credential("grok-sub:work");
        assert_eq!(metadata.provider, CredentialProvider::GrokSubscription);
        assert_eq!(metadata.account.as_deref(), Some("acct-work"));
        assert_eq!(metadata.secret_ref, "oauth-store:grok-sub:work");
        assert!(!format!("{tokens:?}").contains("access-secret"));
    }

    #[test]
    fn rfc8628_poll_states_are_status_and_error_code_only_without_body_reflection() {
        let dialect = XaiGrokOAuthDialect::for_test(
            "http://127.0.0.1:12345",
            Arc::new(FixedVerifier(claims())),
        )
        .unwrap();
        let pending = dialect
            .parse_device_poll(
                StatusCode::BAD_REQUEST,
                json!({"error": "authorization_pending", "secret": "body-sentinel"}),
            )
            .unwrap();
        assert!(matches!(pending, DevicePoll::Pending));
        let slower = dialect
            .parse_device_poll(
                StatusCode::BAD_REQUEST,
                json!({"error": "slow_down", "secret": "body-sentinel"}),
            )
            .unwrap();
        assert!(matches!(slower, DevicePoll::SlowDown));
        let denied = dialect
            .parse_device_poll(
                StatusCode::BAD_REQUEST,
                json!({"error": "access_denied", "secret": "body-sentinel"}),
            )
            .unwrap();
        assert!(matches!(denied, DevicePoll::Denied));
        let expired = dialect
            .parse_device_poll(
                StatusCode::BAD_REQUEST,
                json!({"error": "expired_token", "secret": "body-sentinel"}),
            )
            .unwrap();
        assert!(matches!(expired, DevicePoll::Expired));

        let rejected = match dialect.parse_device_poll(
            StatusCode::BAD_REQUEST,
            json!({"error": "server_error", "secret": "body-sentinel"}),
        ) {
            Ok(_) => panic!("unexpected successful poll response"),
            Err(error) => error.to_string(),
        };
        assert!(rejected.contains("HTTP 400"), "{rejected}");
        assert!(!rejected.contains("body-sentinel"), "{rejected}");
        assert!(!rejected.contains("server_error"), "{rejected}");
    }

    #[test]
    fn device_authorization_start_errors_are_status_only_and_404_is_actionable() {
        let dialect = XaiGrokOAuthDialect::for_test(
            "http://127.0.0.1:12345",
            Arc::new(FixedVerifier(claims())),
        )
        .unwrap();
        let forbidden = dialect
            .parse_device_authorization(
                StatusCode::FORBIDDEN,
                json!({"error": "policy-secret-sentinel"}),
            )
            .unwrap_err()
            .to_string();
        assert!(forbidden.contains("HTTP 403"), "{forbidden}");
        assert!(!forbidden.contains("policy-secret-sentinel"), "{forbidden}");

        let missing = dialect
            .parse_device_authorization(
                StatusCode::NOT_FOUND,
                json!({"error": "unsupported-secret-sentinel"}),
            )
            .unwrap_err();
        assert!(matches!(
            missing.downcast_ref::<GrokDeviceEndpointError>(),
            Some(GrokDeviceEndpointError::StartDisabledOrUnsupported)
        ));
        let missing = missing.to_string();
        assert!(missing.contains("disabled or unsupported"), "{missing}");
        assert!(missing.contains("API key"), "{missing}");
        assert!(
            !missing.contains("unsupported-secret-sentinel"),
            "{missing}"
        );

        let rejected = dialect
            .parse_device_authorization(
                StatusCode::UNAUTHORIZED,
                json!({"error": "invalid_client", "error_description": "Unknown or disabled client"}),
            )
            .unwrap_err();
        assert!(matches!(
            rejected.downcast_ref::<GrokDeviceEndpointError>(),
            Some(GrokDeviceEndpointError::ClientRejected)
        ));
        assert!(!rejected.to_string().contains("Unknown or disabled"));
    }

    #[tokio::test]
    async fn grok_token_claim_mismatch_matrix_fails_before_record_creation() {
        for defect in [
            "issuer",
            "audience",
            "multi-audience",
            "account-control",
            "account-length",
            "nonce",
            "expired",
            "not_before",
        ] {
            let mut claims = claims();
            let context = if defect == "nonce" {
                claims.nonce = Some("wrong".into());
                TokenValidationContext::Browser {
                    expected_nonce: "expected".into(),
                    redirect_uri: "http://127.0.0.1/callback".into(),
                }
            } else {
                match defect {
                    "issuer" => claims.issuer = "https://evil.example".into(),
                    "audience" => claims.audiences = BTreeSet::from(["other-client".into()]),
                    "multi-audience" => {
                        claims.audiences.insert("other-client".into());
                        claims.authorized_party = None;
                    }
                    "account-control" => claims.account_id = "acct\nforged".into(),
                    "account-length" => claims.account_id = "x".repeat(257),
                    "expired" => claims.expires_at = Utc::now() - TimeDelta::minutes(1),
                    "not_before" => {
                        claims.not_before = Some(Utc::now() + TimeDelta::minutes(5));
                    }
                    _ => unreachable!(),
                }
                TokenValidationContext::Device
            };
            let dialect = XaiGrokOAuthDialect::for_test(
                "http://127.0.0.1:12345",
                Arc::new(FixedVerifier(claims)),
            )
            .unwrap();
            let error = dialect
                .validate_tokens(
                    StatusCode::OK,
                    json!({
                        "access_token": "access-secret",
                        "refresh_token": "refresh-secret",
                        "id_token": "id-secret"
                    }),
                    None,
                    &context,
                    &CancellationToken::new(),
                )
                .await
                .unwrap_err();
            let expected = if matches!(defect, "account-control" | "account-length") {
                GrokAuthStageError::AccountEntitlement
            } else {
                GrokAuthStageError::ClientBinding
            };
            assert_eq!(
                error.downcast_ref::<GrokAuthStageError>(),
                Some(&expected),
                "defect={defect} error={error}"
            );
            assert!(!format!("{error:#}").contains("secret"));
        }
    }

    #[tokio::test]
    async fn hostile_token_error_and_unsigned_metadata_never_leak_or_define_authority() {
        let dialect = XaiGrokOAuthDialect::for_test(
            "http://127.0.0.1:12345",
            Arc::new(FixedVerifier(claims())),
        )
        .unwrap();
        let marker = "refresh-secret-echo-marker";
        let error = dialect
            .validate_tokens(
                StatusCode::BAD_REQUEST,
                json!({"error": marker}),
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<GrokAuthStageError>(),
            Some(&GrokAuthStageError::TokenExchangeRejected(400))
        );
        let rendered = error.to_string();
        assert!(rendered.contains("token exchange was rejected (HTTP 400)"));
        assert!(!rendered.contains(marker));

        let mut short = claims();
        short.expires_at = Utc::now() + TimeDelta::minutes(5);
        let signed_expiry = short.expires_at;
        let dialect =
            XaiGrokOAuthDialect::for_test("http://127.0.0.1:12345", Arc::new(FixedVerifier(short)))
                .unwrap();
        let record = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "access_token": "access-secret",
                    "refresh_token": "refresh-secret",
                    "expires_in": "unrequested-metadata",
                    "scope": "openid profile email offline_access"
                }),
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(record.expires_at, signed_expiry);
        assert_eq!(record.scopes, grok_required_scopes());
    }

    struct MemoryStore(Mutex<BTreeMap<String, OAuthTokenRecord>>);

    impl OAuthCredentialStore for MemoryStore {
        fn load(&self, reference: &str) -> Result<Option<OAuthTokenRecord>> {
            Ok(self.0.lock().unwrap().get(reference).cloned())
        }

        fn compare_and_swap(
            &self,
            reference: &str,
            expected_generation: Option<&str>,
            replacement: &OAuthTokenRecord,
        ) -> Result<()> {
            let mut records = self.0.lock().unwrap();
            if records
                .get(reference)
                .map(|record| record.generation.as_str())
                != expected_generation
            {
                bail!("generation mismatch");
            }
            records.insert(reference.into(), replacement.clone());
            Ok(())
        }
    }

    struct FakeReply {
        status: StatusCode,
        body: String,
    }

    #[derive(Default)]
    struct FakeState {
        replies: Mutex<BTreeMap<String, std::collections::VecDeque<FakeReply>>>,
        requests: Mutex<Vec<(String, String)>>,
    }

    struct FakeServer {
        origin: String,
        state: Arc<FakeState>,
        task: tokio::task::JoinHandle<()>,
    }

    impl FakeServer {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let state = Arc::new(FakeState::default());
            let app = axum::Router::new()
                .fallback(axum::routing::post(fake_handler))
                .with_state(state.clone());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                origin,
                state,
                task,
            }
        }

        fn push(&self, path: &str, status: StatusCode, body: Value) {
            self.state
                .replies
                .lock()
                .unwrap()
                .entry(path.into())
                .or_default()
                .push_back(FakeReply {
                    status,
                    body: body.to_string(),
                });
        }

        fn request_bodies(&self, path: &str) -> Vec<String> {
            self.state
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(actual, _)| actual == path)
                .map(|(_, body)| body.clone())
                .collect()
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn fake_handler(
        axum::extract::State(state): axum::extract::State<Arc<FakeState>>,
        request: axum::extract::Request,
    ) -> axum::http::Response<axum::body::Body> {
        let path = request.uri().path().to_string();
        let body = axum::body::to_bytes(request.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body).into_owned();
        state.requests.lock().unwrap().push((path.clone(), body));
        let reply = state
            .replies
            .lock()
            .unwrap()
            .get_mut(&path)
            .and_then(|queue| queue.pop_front())
            .unwrap_or(FakeReply {
                status: StatusCode::NOT_FOUND,
                body: json!({"error": "unexpected-path"}).to_string(),
            });
        axum::http::Response::builder()
            .status(reply.status)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(reply.body))
            .unwrap()
    }

    #[tokio::test]
    async fn fake_service_login_success_refresh_revoke_and_redaction() {
        let server = FakeServer::start().await;
        let dialect = Arc::new(
            XaiGrokOAuthDialect::for_test(&server.origin, Arc::new(FixedVerifier(claims())))
                .unwrap(),
        );
        let store = Arc::new(MemoryStore(Mutex::new(BTreeMap::new())));
        let client = crate::oauth::OAuthClient::new(dialect, store.clone()).unwrap();

        server.push(
            "/oauth2/device/code",
            StatusCode::OK,
            json!({
                "device_code": "device-secret",
                "user_code": "WXYZ-1234",
                "verification_uri": format!("{}/device", server.origin),
                "expires_in": 600,
                "interval": 0
            }),
        );
        server.push(
            "/oauth2/token",
            StatusCode::OK,
            json!({
                "access_token": "access-secret",
                "refresh_token": "refresh-secret",
                "expires_in": 3600
            }),
        );
        let pending = client.begin_device_authorization().await.unwrap();
        assert_eq!(pending.user_code, "WXYZ-1234");
        assert!(!format!("{pending:?}").contains("device-secret"));
        let credential = client
            .finish_device_authorization("grok-sub:work", &pending, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(credential.provider, CredentialProvider::GrokSubscription);
        assert_eq!(credential.account.as_deref(), Some("acct-work"));

        let start_body = &server.request_bodies("/oauth2/device/code")[0];
        assert!(start_body.contains("client_id"));
        assert!(start_body.contains("referrer=finch") || start_body.contains("referrer=finch"));
        assert!(!start_body.contains("api.x.ai"));

        server.push(
            "/oauth2/token",
            StatusCode::OK,
            json!({
                "access_token": "rotated-access",
                "refresh_token": "rotated-refresh",
                "expires_in": 3600
            }),
        );
        client
            .refresh("grok-sub:work", CancellationToken::new())
            .await
            .unwrap();
        let stored = store.load("grok-sub:work").unwrap().unwrap();
        assert_eq!(stored.access_token, "rotated-access");
        assert!(!format!("{stored:?}").contains("rotated-access"));

        server.push("/oauth2/revoke", StatusCode::OK, json!({}));
        let revoked = client
            .revoke("grok-sub:work", CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(
            revoked.lifecycle,
            crate::CredentialLifecycle::Revoked
        ));
        let tombstone = store.load("grok-sub:work").unwrap().unwrap();
        assert!(tombstone.revoked);
        assert!(tombstone.access_token.is_empty());
    }

    #[tokio::test]
    async fn fake_service_pending_denial_expiry_cancellation_and_malformed() {
        let server = FakeServer::start().await;
        let dialect = Arc::new(
            XaiGrokOAuthDialect::for_test(&server.origin, Arc::new(FixedVerifier(claims())))
                .unwrap(),
        );
        let store = Arc::new(MemoryStore(Mutex::new(BTreeMap::new())));
        let client = crate::oauth::OAuthClient::new(dialect.clone(), store.clone()).unwrap();

        server.push(
            "/oauth2/device/code",
            StatusCode::OK,
            json!({
                "device_code": "device-denied",
                "user_code": "DENY-0001",
                "verification_uri": format!("{}/device", server.origin),
                "expires_in": 600,
                "interval": 0
            }),
        );
        server.push(
            "/oauth2/token",
            StatusCode::BAD_REQUEST,
            json!({"error": "authorization_pending"}),
        );
        server.push(
            "/oauth2/token",
            StatusCode::BAD_REQUEST,
            json!({"error": "access_denied", "error_description": "user-secret"}),
        );
        let pending = client.begin_device_authorization().await.unwrap();
        let denied = client
            .finish_device_authorization("grok-sub:denied", &pending, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(denied.to_string().contains("denied"));
        assert!(!denied.to_string().contains("user-secret"));
        assert!(store.load("grok-sub:denied").unwrap().is_none());

        server.push(
            "/oauth2/device/code",
            StatusCode::OK,
            json!({
                "device_code": "device-expired",
                "user_code": "EXPI-0001",
                "verification_uri": format!("{}/device", server.origin),
                "expires_in": 600,
                "interval": 0
            }),
        );
        server.push(
            "/oauth2/token",
            StatusCode::BAD_REQUEST,
            json!({"error": "expired_token"}),
        );
        let pending = client.begin_device_authorization().await.unwrap();
        let expired = client
            .finish_device_authorization("grok-sub:expired", &pending, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(expired.to_string().contains("expired"));
        assert!(store.load("grok-sub:expired").unwrap().is_none());

        server.push(
            "/oauth2/device/code",
            StatusCode::OK,
            json!({
                "device_code": "device-cancel",
                "user_code": "CANC-0001",
                "verification_uri": format!("{}/device", server.origin),
                "expires_in": 600,
                "interval": 0
            }),
        );
        let pending = client.begin_device_authorization().await.unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let cancelled = client
            .finish_device_authorization("grok-sub:cancel", &pending, cancel)
            .await
            .unwrap_err();
        assert!(cancelled.to_string().contains("cancelled"));
        assert!(store.load("grok-sub:cancel").unwrap().is_none());

        server.push(
            "/oauth2/device/code",
            StatusCode::OK,
            json!({
                "device_code": "device-malformed",
                "user_code": "MALF-0001",
                "verification_uri": format!("{}/device", server.origin),
                "expires_in": 600,
                "interval": 0
            }),
        );
        server.push("/oauth2/token", StatusCode::OK, json!("not-an-object"));
        let pending = client.begin_device_authorization().await.unwrap();
        let malformed = client
            .finish_device_authorization("grok-sub:malformed", &pending, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(!malformed.to_string().contains("access-secret"));
        assert!(store.load("grok-sub:malformed").unwrap().is_none());
    }

    #[tokio::test]
    async fn restart_after_revocation_replaces_tombstone_without_resurrecting_secret() {
        let server = FakeServer::start().await;
        let dialect = Arc::new(
            XaiGrokOAuthDialect::for_test(&server.origin, Arc::new(FixedVerifier(claims())))
                .unwrap(),
        );
        let store = Arc::new(MemoryStore(Mutex::new(BTreeMap::new())));
        let client = crate::oauth::OAuthClient::new(dialect, store.clone()).unwrap();

        server.push(
            "/oauth2/device/code",
            StatusCode::OK,
            json!({
                "device_code": "device-first",
                "user_code": "FIRS-0001",
                "verification_uri": format!("{}/device", server.origin),
                "expires_in": 600,
                "interval": 0
            }),
        );
        server.push(
            "/oauth2/token",
            StatusCode::OK,
            json!({
                "access_token": "first-access",
                "refresh_token": "first-refresh",
                "expires_in": 3600
            }),
        );
        let pending = client.begin_device_authorization().await.unwrap();
        client
            .finish_device_authorization("grok-sub:work", &pending, CancellationToken::new())
            .await
            .unwrap();
        server.push("/oauth2/revoke", StatusCode::OK, json!({}));
        client
            .revoke("grok-sub:work", CancellationToken::new())
            .await
            .unwrap();
        let tombstone = store.load("grok-sub:work").unwrap().unwrap();
        assert!(tombstone.revoked);
        assert!(tombstone.access_token.is_empty());

        server.push(
            "/oauth2/device/code",
            StatusCode::OK,
            json!({
                "device_code": "device-second",
                "user_code": "SECO-0001",
                "verification_uri": format!("{}/device", server.origin),
                "expires_in": 600,
                "interval": 0
            }),
        );
        server.push(
            "/oauth2/token",
            StatusCode::OK,
            json!({
                "access_token": "second-access",
                "refresh_token": "second-refresh",
                "expires_in": 3600
            }),
        );
        let pending = client.begin_device_authorization().await.unwrap();
        client
            .finish_device_authorization("grok-sub:work", &pending, CancellationToken::new())
            .await
            .unwrap();
        let restored = store.load("grok-sub:work").unwrap().unwrap();
        assert!(!restored.revoked);
        assert_eq!(restored.access_token, "second-access");
        assert_ne!(restored.generation, tombstone.generation);
        assert!(!format!("{restored:?}").contains("first-access"));
        assert!(!format!("{restored:?}").contains("second-access"));
    }
}
