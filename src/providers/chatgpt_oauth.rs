//! OpenAI ChatGPT subscription OAuth compatibility dialect.
//!
//! This adapter reproduces the public-client protocol visible in the
//! open-source Codex client. It is not the OpenAI Platform API and tokens from
//! it must never be sent to `api.openai.com`. The compatibility revision is a
//! fail-closed fence, not a claim of a stable third-party contract.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{TimeDelta, Utc};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::{AudienceBinding, CredentialKind, CredentialProvider, EndpointFamily};
use crate::oauth::{
    validate_secret_field, AuthorizationCodeGrant, DeviceAuthorization, DevicePoll, OAuthDialect,
    OAuthDialectDescriptor, OAuthHttpRequest, OAuthRequestBody, OAuthTokenRecord,
    TokenValidationContext,
};

pub const CHATGPT_OAUTH_PROTOCOL_REVISION: &str =
    "openai-codex-public-client@3e4707b34b16e139fcb7ad11ab8445993b62bba1";
pub const CHATGPT_SUBSCRIPTION_SERVICE_REVISION: &str =
    "chatgpt-codex-service@6478a751fde8884b2fdc76486fe23175a8e795d4";
pub(crate) const OPENAI_PUBLIC_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(crate) const OPENAI_AUTH_ORIGIN: &str = "https://auth.openai.com";
const CHATGPT_SERVICE_ORIGIN: &str = "https://chatgpt.com";
pub const CHATGPT_SUBSCRIPTION_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub(crate) const REQUIRED_TOKEN_ISSUER: &str = "https://auth.openai.com";
/// Access-token `aud` minted by the pinned Codex public-client revision. This
/// is token authority metadata, not permission to send the token to that URL.
pub(crate) const OPENAI_CODEX_ACCESS_TOKEN_AUDIENCE: &str = "https://api.openai.com/v1";
const CHATGPT_AUTH_CLAIM_NAMESPACE: &str = "https://api.openai.com/auth";
const DEVICE_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// Exact scopes for the pinned public-client compatibility revision.
pub fn chatgpt_required_scopes() -> BTreeSet<String> {
    BTreeSet::from([
        "openid".into(),
        "profile".into(),
        "email".into(),
        "offline_access".into(),
        "api.connectors.read".into(),
        "api.connectors.invoke".into(),
    ])
}

/// Signature-verified provider claims. The adapter does not parse an
/// unverified JWT payload and call it identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOpenAiClaims {
    pub issuer: String,
    pub audiences: BTreeSet<String>,
    pub authorized_party: Option<String>,
    pub access_audiences: BTreeSet<String>,
    pub subject: String,
    pub account_id: String,
    /// Verified `https://api.openai.com/auth.chatgpt_plan_type` entitlement.
    pub chatgpt_plan_type: String,
    /// Verified `https://api.openai.com/auth.chatgpt_account_is_fedramp` routing claim.
    pub account_is_fedramp: bool,
    pub scopes: BTreeSet<String>,
    pub nonce: Option<String>,
    /// Signature-verified JWT `exp` authority.
    pub expires_at: chrono::DateTime<Utc>,
    /// Signature-verified JWT `nbf` authority when present.
    pub not_before: Option<chrono::DateTime<Utc>>,
}

/// Injected JWS/JWKS verification boundary. Production enablement must supply
/// an implementation pinned to the dialect's exact issuer and algorithms.
#[async_trait]
pub trait OpenAiTokenVerifier: Send + Sync {
    fn preflight(&self) -> Result<()>;
    async fn verify(
        &self,
        id_token: Option<&str>,
        access_token: &str,
        cancel: &CancellationToken,
    ) -> Result<VerifiedOpenAiClaims>;
}

/// Fail-closed default until Finch's audited OpenAI JWKS verifier is wired.
#[derive(Debug, Default)]
pub struct OpenAiVerificationUnavailable;

#[async_trait]
impl OpenAiTokenVerifier for OpenAiVerificationUnavailable {
    fn preflight(&self) -> Result<()> {
        unavailable_verifier_error()
    }

    async fn verify(
        &self,
        _id_token: Option<&str>,
        _access_token: &str,
        _cancel: &CancellationToken,
    ) -> Result<VerifiedOpenAiClaims> {
        unavailable_verifier_error()
    }
}

fn unavailable_verifier_error<T>() -> Result<T> {
    bail!("ChatGPT OAuth token verification is unavailable for this compatibility revision; update Finch rather than bypassing issuer or signature validation")
}

/// Strict OpenAI-specific dialect; reusable OAuth state remains in `oauth`.
pub struct OpenAiChatGptOAuthDialect<V> {
    descriptor: OAuthDialectDescriptor,
    verifier: Arc<V>,
    auth_origin: String,
}

impl OpenAiChatGptOAuthDialect<crate::providers::openai_jwks::OpenAiJwksVerifier> {
    pub fn production() -> Result<Self> {
        Self::new(
            OPENAI_AUTH_ORIGIN,
            Arc::new(crate::providers::openai_jwks::OpenAiJwksVerifier::production()?),
            false,
        )
    }
}

impl<V> OpenAiChatGptOAuthDialect<V>
where
    V: OpenAiTokenVerifier,
{
    fn new(auth_origin: &str, verifier: Arc<V>, allow_insecure_loopback: bool) -> Result<Self> {
        let auth_origin = auth_origin.trim_end_matches('/').to_string();
        let allowed_origins = BTreeSet::from([auth_origin.clone()]);
        let descriptor = OAuthDialectDescriptor {
            dialect_id: "openai_chatgpt_subscription".into(),
            protocol_revision: CHATGPT_OAUTH_PROTOCOL_REVISION.into(),
            provider: CredentialProvider::ChatgptSubscription,
            credential_kind: CredentialKind::OauthDevice,
            browser_credential_kind: None,
            // This is #174's locally enforced provider issuer descriptor. The
            // signed token issuer is independently required below.
            issuer: "openai-chatgpt".into(),
            audience: AudienceBinding::standard(EndpointFamily::ChatgptSubscription),
            client_id: OPENAI_PUBLIC_CLIENT_ID.into(),
            scopes: chatgpt_required_scopes(),
            device_authorization_endpoint: format!(
                "{auth_origin}/api/accounts/deviceauth/usercode"
            ),
            device_token_endpoint: format!("{auth_origin}/api/accounts/deviceauth/token"),
            authorization_endpoint: format!("{auth_origin}/oauth/authorize"),
            token_endpoint: format!("{auth_origin}/oauth/token"),
            revocation_endpoint: format!("{auth_origin}/oauth/revoke"),
            allowed_origins,
            allowed_user_authorization_origins: BTreeSet::from([auth_origin.clone()]),
            allow_insecure_loopback,
        };
        descriptor.validate()?;
        Ok(Self {
            descriptor,
            verifier,
            auth_origin,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(auth_origin: &str, verifier: Arc<V>) -> Result<Self> {
        Self::new(auth_origin, verifier, true)
    }
}

#[async_trait]
impl<V> OAuthDialect for OpenAiChatGptOAuthDialect<V>
where
    V: OpenAiTokenVerifier + 'static,
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
            body: OAuthRequestBody::Json(json!({
                "client_id": self.descriptor.client_id,
            })),
        })
    }

    fn parse_device_authorization(
        &self,
        status: StatusCode,
        body: Value,
    ) -> Result<DeviceAuthorization> {
        if !status.is_success() {
            bail!("ChatGPT device authorization is unavailable (HTTP {status})");
        }
        let device_code = required_string(&body, "device_auth_id")?;
        let user_code = body
            .get("user_code")
            .or_else(|| body.get("usercode"))
            .and_then(Value::as_str)
            .context("ChatGPT device authorization response omitted user_code")?
            .to_string();
        validate_secret_field(&user_code, "user code")?;
        let interval = body
            .get("interval")
            .and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_str()?.parse::<u64>().ok())
            })
            .context("ChatGPT device authorization response has invalid interval")?;
        DeviceAuthorization::issued(
            device_code,
            user_code,
            format!("{}/codex/device", self.auth_origin),
            None,
            DEVICE_LIFETIME,
            Duration::from_secs(interval),
        )
    }

    fn device_poll_request(&self, pending: &DeviceAuthorization) -> Result<OAuthHttpRequest> {
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.device_token_endpoint.clone(),
            body: OAuthRequestBody::Json(json!({
                "device_auth_id": pending.device_code,
                "user_code": pending.user_code,
            })),
        })
    }

    fn parse_device_poll(&self, status: StatusCode, body: Value) -> Result<DevicePoll> {
        if status.is_success() {
            let code = required_string(&body, "authorization_code")?;
            let verifier = required_string(&body, "code_verifier")?;
            let challenge = required_string(&body, "code_challenge")?;
            if URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != challenge {
                bail!("ChatGPT device authorization returned an invalid PKCE pair");
            }
            return Ok(DevicePoll::AuthorizationCode(AuthorizationCodeGrant {
                code,
                verifier,
                redirect_uri: format!("{}/deviceauth/callback", self.auth_origin),
            }));
        }
        let code = body.get("error").and_then(Value::as_str).unwrap_or("");
        match code {
            "authorization_pending" | "pending" => Ok(DevicePoll::Pending),
            "slow_down" => Ok(DevicePoll::SlowDown),
            "access_denied" | "authorization_declined" => Ok(DevicePoll::Denied),
            "expired_token" | "authorization_expired" => Ok(DevicePoll::Expired),
            _ if matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN)
                && body.as_object().is_some_and(|object| object.is_empty()) =>
            {
                Ok(DevicePoll::Pending)
            }
            _ => bail!("ChatGPT device polling contract changed (HTTP {status})"),
        }
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
            body: OAuthRequestBody::Json(json!({
                "client_id": self.descriptor.client_id,
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
            })),
        })
    }

    fn revoke_request(&self, token: &str) -> Result<OAuthHttpRequest> {
        validate_secret_field(token, "revocation token")?;
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.revocation_endpoint.clone(),
            body: OAuthRequestBody::Json(json!({
                "client_id": self.descriptor.client_id,
                "token": token,
                "token_type_hint": "refresh_token",
            })),
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
            let code = safe_oauth_error_code(&body);
            bail!("ChatGPT token request failed (HTTP {status}, code {code})");
        }
        let access_token = required_string(&body, "access_token")?;
        let refresh_token = body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| previous.and_then(|record| record.refresh_token.clone()))
            .context("ChatGPT token response omitted a refresh token")?;
        validate_secret_field(&refresh_token, "refresh token")?;
        let id_token = body
            .get("id_token")
            .and_then(Value::as_str)
            .map(str::to_string);
        let claims = self
            .verifier
            .verify(id_token.as_deref(), &access_token, cancel)
            .await?;
        let now = Utc::now();
        if claims.issuer != REQUIRED_TOKEN_ISSUER
            || !claims.audiences.contains(&self.descriptor.client_id)
            || (claims.audiences.len() > 1
                && claims.authorized_party.as_deref() != Some(self.descriptor.client_id.as_str()))
            || claims
                .authorized_party
                .as_deref()
                .is_some_and(|party| party != self.descriptor.client_id)
            || claims.access_audiences
                != BTreeSet::from([OPENAI_CODEX_ACCESS_TOKEN_AUDIENCE.to_string()])
            || !self.descriptor.scopes.is_subset(&claims.scopes)
            || claims.expires_at <= now
            || claims.not_before.is_some_and(|not_before| not_before > now)
        {
            bail!("ChatGPT token issuer, audience, account, scope, or lifetime validation failed");
        }
        validate_public_claim(&claims.subject, "subject")?;
        validate_public_claim(&claims.account_id, "account identifier")?;
        validate_public_claim(&claims.chatgpt_plan_type, "plan type")?;
        match context {
            TokenValidationContext::Browser { expected_nonce, .. }
                if claims.nonce.as_deref() != Some(expected_nonce.as_str()) =>
            {
                bail!("ChatGPT browser token nonce mismatch")
            }
            TokenValidationContext::Refresh if previous.is_none() => {
                bail!("ChatGPT refresh token validation lacks prior account authority")
            }
            _ => {}
        }
        if let Some(previous) = previous {
            if previous.account != claims.account_id {
                bail!("ChatGPT token refresh changed accounts");
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
            scopes: claims.scopes,
            access_token,
            refresh_token: Some(refresh_token),
            id_token,
            expires_at: claims.expires_at,
            generation: Uuid::new_v4().to_string(),
            revoked: false,
            mutation_pending: false,
        })
    }
}

fn validate_public_claim(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("ChatGPT signed {label} is invalid");
    }
    Ok(())
}

fn safe_oauth_error_code(body: &Value) -> &'static str {
    match body.get("error").and_then(Value::as_str) {
        Some("invalid_request") => "invalid_request",
        Some("invalid_client") => "invalid_client",
        Some("invalid_grant") => "invalid_grant",
        Some("unauthorized_client") => "unauthorized_client",
        Some("unsupported_grant_type") => "unsupported_grant_type",
        Some("invalid_scope") => "invalid_scope",
        Some("access_denied") => "access_denied",
        Some("expired_token") => "expired_token",
        Some("temporarily_unavailable") => "temporarily_unavailable",
        Some("server_error") => "server_error",
        _ => "unrecognized",
    }
}

fn required_string(body: &Value, field: &str) -> Result<String> {
    let value = body
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("ChatGPT response omitted {field}"))?
        .to_string();
    validate_secret_field(&value, field)?;
    Ok(value)
}

/// Secret-free subscription service authority used by transport wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptSubscriptionService {
    pub protocol_revision: &'static str,
    pub base_url: &'static str,
}

impl Default for ChatGptSubscriptionService {
    fn default() -> Self {
        Self {
            protocol_revision: CHATGPT_SUBSCRIPTION_SERVICE_REVISION,
            base_url: CHATGPT_SUBSCRIPTION_BASE_URL,
        }
    }
}

impl ChatGptSubscriptionService {
    /// Reject Platform and custom origin substitution before a bearer token is
    /// available to the transport.
    pub fn validate_endpoint(&self, endpoint: &str) -> Result<()> {
        let requested = reqwest::Url::parse(endpoint)?;
        let required = reqwest::Url::parse(self.base_url)?;
        let required_path = required.path().trim_end_matches('/');
        let path_matches = requested.path() == required_path
            || requested
                .path()
                .strip_prefix(required_path)
                .is_some_and(|suffix| suffix.starts_with('/'));
        if requested.scheme() != "https"
            || requested.host_str() != Some("chatgpt.com")
            || requested.port_or_known_default() != required.port_or_known_default()
            || !path_matches
            || requested.username() != ""
            || requested.password().is_some()
            || requested.fragment().is_some()
        {
            bail!("ChatGPT subscription credentials may only use the versioned chatgpt.com subscription service; OpenAI Platform and custom endpoints require distinct credentials");
        }
        Ok(())
    }

    pub fn validate_account_header(&self, record: &OAuthTokenRecord, account: &str) -> Result<()> {
        if record.provider != CredentialProvider::ChatgptSubscription
            || record.audience != AudienceBinding::standard(EndpointFamily::ChatgptSubscription)
            || record.account != account
        {
            bail!("ChatGPT subscription request account does not match its named credential");
        }
        Ok(())
    }

    pub fn parse_catalog(&self, account: &str, body: &[u8]) -> Result<ChatGptCatalog> {
        if body.len() > 64 * 1024 {
            bail!("ChatGPT subscription catalog exceeded the size limit");
        }
        let wire: CatalogWire = serde_json::from_slice(body)
            .context("ChatGPT subscription catalog contract changed")?;
        if wire.account_id != account || wire.models.is_empty() || wire.models.len() > 512 {
            bail!("ChatGPT subscription catalog account or model allowance is invalid");
        }
        let mut models = Vec::with_capacity(wire.models.len());
        for model in wire.models {
            if model.slug.is_empty()
                || model.slug.len() > 256
                || model.slug.chars().any(char::is_control)
                || !model.supported_in_api
            {
                bail!("ChatGPT subscription catalog advertised an unusable model");
            }
            models.push(model.slug);
        }
        Ok(ChatGptCatalog {
            account_id: wire.account_id,
            models,
            allowance: wire.allowance,
        })
    }

    pub fn actual_model(
        &self,
        requested: &str,
        response_model: Option<&str>,
        header_model: Option<&str>,
    ) -> Result<String> {
        let actual = header_model
            .or(response_model)
            .context("ChatGPT subscription response omitted actual model provenance")?;
        if requested.trim().is_empty()
            || actual.is_empty()
            || actual.len() > 256
            || actual.chars().any(char::is_control)
        {
            bail!("ChatGPT subscription model provenance is invalid");
        }
        Ok(actual.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptCatalog {
    pub account_id: String,
    pub models: Vec<String>,
    pub allowance: Option<String>,
}

#[derive(Deserialize)]
struct CatalogWire {
    account_id: String,
    models: Vec<CatalogModelWire>,
    #[serde(default)]
    allowance: Option<String>,
}

#[derive(Deserialize)]
struct CatalogModelWire {
    slug: String,
    #[serde(default)]
    supported_in_api: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct NoopStore;

    impl crate::oauth::OAuthCredentialStore for NoopStore {
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
    struct FixedVerifier(VerifiedOpenAiClaims);

    #[async_trait]
    impl OpenAiTokenVerifier for FixedVerifier {
        fn preflight(&self) -> Result<()> {
            Ok(())
        }

        async fn verify(
            &self,
            _id_token: Option<&str>,
            _access_token: &str,
            _cancel: &CancellationToken,
        ) -> Result<VerifiedOpenAiClaims> {
            Ok(self.0.clone())
        }
    }

    fn claims() -> VerifiedOpenAiClaims {
        VerifiedOpenAiClaims {
            issuer: REQUIRED_TOKEN_ISSUER.into(),
            audiences: BTreeSet::from([OPENAI_PUBLIC_CLIENT_ID.into()]),
            authorized_party: None,
            access_audiences: BTreeSet::from([OPENAI_CODEX_ACCESS_TOKEN_AUDIENCE.into()]),
            subject: "subject-work".into(),
            account_id: "acct-work".into(),
            chatgpt_plan_type: "plus".into(),
            account_is_fedramp: false,
            scopes: BTreeSet::from([
                "openid".into(),
                "profile".into(),
                "email".into(),
                "offline_access".into(),
                "api.connectors.read".into(),
                "api.connectors.invoke".into(),
            ]),
            nonce: None,
            expires_at: Utc::now() + TimeDelta::hours(1),
            not_before: None,
        }
    }

    fn record() -> OAuthTokenRecord {
        OAuthTokenRecord {
            dialect_id: "openai_chatgpt_subscription".into(),
            protocol_revision: CHATGPT_OAUTH_PROTOCOL_REVISION.into(),
            provider: CredentialProvider::ChatgptSubscription,
            kind: CredentialKind::OauthDevice,
            issuer: "openai-chatgpt".into(),
            audience: AudienceBinding::standard(EndpointFamily::ChatgptSubscription),
            client_id: OPENAI_PUBLIC_CLIENT_ID.into(),
            account: "acct-work".into(),
            tenant: None,
            project: None,
            scopes: claims().scopes,
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
    fn subscription_service_never_crosses_to_platform_or_silent_api_key_fallback() {
        let service = ChatGptSubscriptionService::default();
        service
            .validate_endpoint("https://chatgpt.com/backend-api/codex/models")
            .unwrap();
        for hostile in [
            "https://api.openai.com/v1/models",
            "https://chatgpt.com.evil.example/backend-api/codex/models",
            "https://chatgpt.com/v1/models",
            "https://chatgpt.com/backend-api/codexevil/models",
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
        platform.provider = CredentialProvider::OpenaiPlatform;
        assert!(service
            .validate_account_header(&platform, "acct-work")
            .is_err());
    }

    #[tokio::test]
    async fn unavailable_production_verifier_fails_client_preflight_before_any_socket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let dialect =
            OpenAiChatGptOAuthDialect::new(&origin, Arc::new(OpenAiVerificationUnavailable), true)
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

    #[test]
    fn fake_subscription_catalog_binds_account_allowance_and_actual_model_provenance() {
        let service = ChatGptSubscriptionService::default();
        let body = br#"{"account_id":"acct-work","allowance":"subscription","models":[{"slug":"gpt-5.6-sol","supported_in_api":true}]}"#;
        let catalog = service.parse_catalog("acct-work", body).unwrap();
        assert_eq!(catalog.allowance.as_deref(), Some("subscription"));
        assert_eq!(catalog.models, ["gpt-5.6-sol"]);
        assert!(service.parse_catalog("acct-other", body).is_err());
        assert_eq!(
            service
                .actual_model(
                    "gpt-5.6-sol",
                    Some("gpt-5.6-sol"),
                    Some("gpt-5.6-sol-safety-routed")
                )
                .unwrap(),
            "gpt-5.6-sol-safety-routed"
        );
        assert!(service.actual_model("gpt-5.6-sol", None, None).is_err());
    }

    #[tokio::test]
    async fn public_client_risk_is_versioned_and_verification_is_exactly_pinned() {
        let dialect = OpenAiChatGptOAuthDialect::production().unwrap();
        assert!(dialect
            .descriptor()
            .protocol_revision
            .contains("3e4707b34b16"));
        assert!(!dialect
            .descriptor()
            .allowed_origins
            .contains("https://api.openai.com"));
        assert_eq!(
            dialect.descriptor().authorization_endpoint,
            "https://auth.openai.com/oauth/authorize"
        );
        assert!(dialect.descriptor().browser_credential_kind.is_none());
        assert_eq!(dialect.descriptor().scopes.len(), 6);
        assert_eq!(CHATGPT_AUTH_CLAIM_NAMESPACE, "https://api.openai.com/auth");
        assert!(dialect.preflight().is_ok());
    }

    #[tokio::test]
    async fn openai_dialect_exact_device_pkce_and_token_fixture_is_strictly_bound() {
        let dialect = OpenAiChatGptOAuthDialect::for_test(
            "http://127.0.0.1:12345",
            Arc::new(FixedVerifier(claims())),
        )
        .unwrap();
        let request = dialect.device_authorization_request().unwrap();
        assert_eq!(
            request.endpoint,
            "http://127.0.0.1:12345/api/accounts/deviceauth/usercode"
        );
        assert_eq!(
            request.body,
            OAuthRequestBody::Json(json!({"client_id": OPENAI_PUBLIC_CLIENT_ID}))
        );
        let _pending = dialect
            .parse_device_authorization(
                StatusCode::OK,
                json!({
                    "device_auth_id": "device-secret",
                    "user_code": "ABCD-EFGH",
                    "interval": "1"
                }),
            )
            .unwrap();
        let verifier = "pkce-verifier";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let poll = dialect
            .parse_device_poll(
                StatusCode::OK,
                json!({
                    "authorization_code": "authorization-secret",
                    "code_verifier": verifier,
                    "code_challenge": challenge
                }),
            )
            .unwrap();
        assert!(matches!(poll, DevicePoll::AuthorizationCode(_)));
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
        let metadata = tokens.provider_credential("chatgpt:work");
        assert_eq!(metadata.provider, CredentialProvider::ChatgptSubscription);
        assert_eq!(metadata.account.as_deref(), Some("acct-work"));
        assert_eq!(metadata.secret_ref, "oauth-store:chatgpt:work");
        assert!(!format!("{tokens:?}").contains("access-secret"));
    }

    #[tokio::test]
    async fn openai_token_claim_mismatch_matrix_fails_before_record_creation() {
        for defect in [
            "issuer",
            "audience",
            "multi-audience",
            "access-audience-extra",
            "scope",
            "account",
            "account-control",
            "account-length",
            "plan-control",
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
                    "access-audience-extra" => {
                        claims.access_audiences.insert("other-service".into());
                    }
                    "scope" => {
                        claims.scopes.remove("offline_access");
                    }
                    "account" => claims.account_id.clear(),
                    "account-control" => claims.account_id = "acct\nforged".into(),
                    "account-length" => claims.account_id = "x".repeat(257),
                    "plan-control" => claims.chatgpt_plan_type = "plus\u{1b}[31m".into(),
                    "expired" => claims.expires_at = Utc::now() - TimeDelta::minutes(1),
                    "not_before" => {
                        claims.not_before = Some(Utc::now() + TimeDelta::minutes(5));
                    }
                    _ => unreachable!(),
                }
                TokenValidationContext::Device
            };
            let dialect = OpenAiChatGptOAuthDialect::for_test(
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
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("validation failed")
                    || error.contains("nonce mismatch")
                    || error.contains("signed account")
                    || error.contains("signed plan"),
                "defect={defect} error={error}"
            );
            assert!(!error.contains("secret"));
        }
    }

    #[tokio::test]
    async fn hostile_token_error_and_unsigned_lifetime_never_leak_or_extend_authority() {
        let dialect = OpenAiChatGptOAuthDialect::for_test(
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
            .unwrap_err()
            .to_string();
        assert!(error.contains("unrecognized"));
        assert!(!error.contains(marker));

        let mut short = claims();
        short.expires_at = Utc::now() + TimeDelta::minutes(5);
        let signed_expiry = short.expires_at;
        let dialect = OpenAiChatGptOAuthDialect::for_test(
            "http://127.0.0.1:12345",
            Arc::new(FixedVerifier(short)),
        )
        .unwrap();
        let record = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "access_token": "access-secret",
                    "refresh_token": "refresh-secret",
                    "expires_in": 86400
                }),
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(record.expires_at, signed_expiry);
    }
}
