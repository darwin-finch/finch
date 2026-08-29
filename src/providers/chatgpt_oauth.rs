//! OpenAI ChatGPT subscription OAuth compatibility dialect.
//!
//! This adapter reproduces the public-client protocol visible in the
//! open-source Codex client. It is not the OpenAI Platform API and tokens from
//! it must never be sent to `api.openai.com`. The compatibility revision is a
//! fail-closed fence, not a claim of a stable third-party contract.

use anyhow::{bail, Context, Result};
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
    "chatgpt-codex-service@3e4707b34b16e139fcb7ad11ab8445993b62bba1";
const OPENAI_PUBLIC_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_AUTH_ORIGIN: &str = "https://auth.openai.com";
const CHATGPT_SERVICE_ORIGIN: &str = "https://chatgpt.com";
pub const CHATGPT_SUBSCRIPTION_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const REQUIRED_TOKEN_ISSUER: &str = "https://auth.openai.com";
const DEVICE_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// Signature-verified provider claims. The adapter does not parse an
/// unverified JWT payload and call it identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOpenAiClaims {
    pub issuer: String,
    pub audiences: BTreeSet<String>,
    pub subject: String,
    pub account_id: String,
    pub scopes: BTreeSet<String>,
    pub nonce: Option<String>,
}

/// Injected JWS/JWKS verification boundary. Production enablement must supply
/// an implementation pinned to the dialect's exact issuer and algorithms.
pub trait OpenAiTokenVerifier: Send + Sync {
    fn verify(&self, id_token: Option<&str>, access_token: &str) -> Result<VerifiedOpenAiClaims>;
}

/// Fail-closed default until Finch's audited OpenAI JWKS verifier is wired.
#[derive(Debug, Default)]
pub struct OpenAiVerificationUnavailable;

impl OpenAiTokenVerifier for OpenAiVerificationUnavailable {
    fn verify(&self, _id_token: Option<&str>, _access_token: &str) -> Result<VerifiedOpenAiClaims> {
        bail!("ChatGPT OAuth token verification is unavailable for this compatibility revision; update Finch rather than bypassing issuer or signature validation")
    }
}

/// Strict OpenAI-specific dialect; reusable OAuth state remains in `oauth`.
pub struct OpenAiChatGptOAuthDialect<V> {
    descriptor: OAuthDialectDescriptor,
    verifier: Arc<V>,
    auth_origin: String,
}

impl OpenAiChatGptOAuthDialect<OpenAiVerificationUnavailable> {
    pub fn production() -> Result<Self> {
        Self::new(
            OPENAI_AUTH_ORIGIN,
            Arc::new(OpenAiVerificationUnavailable),
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
            // This is #174's locally enforced provider issuer descriptor. The
            // signed token issuer is independently required below.
            issuer: "openai-chatgpt".into(),
            audience: AudienceBinding::standard(EndpointFamily::ChatgptSubscription),
            client_id: OPENAI_PUBLIC_CLIENT_ID.into(),
            scopes: BTreeSet::from(["openid".into(), "offline_access".into()]),
            device_authorization_endpoint: format!(
                "{auth_origin}/api/accounts/deviceauth/usercode"
            ),
            device_token_endpoint: format!("{auth_origin}/api/accounts/deviceauth/token"),
            authorization_endpoint: format!("{auth_origin}/authorize"),
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
    fn for_test(auth_origin: &str, verifier: Arc<V>) -> Result<Self> {
        Self::new(auth_origin, verifier, true)
    }
}

impl<V> OAuthDialect for OpenAiChatGptOAuthDialect<V>
where
    V: OpenAiTokenVerifier + 'static,
{
    fn descriptor(&self) -> &OAuthDialectDescriptor {
        &self.descriptor
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
        Ok(DeviceAuthorization {
            device_code,
            user_code,
            verification_uri: format!("{}/codex/device", self.auth_origin),
            verification_uri_complete: None,
            expires_in: DEVICE_LIFETIME,
            interval: Duration::from_secs(interval),
        })
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

    fn validate_tokens(
        &self,
        status: StatusCode,
        body: Value,
        previous: Option<&OAuthTokenRecord>,
        context: &TokenValidationContext,
    ) -> Result<OAuthTokenRecord> {
        if !status.is_success() {
            let code = body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
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
        let claims = self.verifier.verify(id_token.as_deref(), &access_token)?;
        if claims.issuer != REQUIRED_TOKEN_ISSUER
            || !claims.audiences.contains(&self.descriptor.client_id)
            || claims.subject.trim().is_empty()
            || claims.account_id.trim().is_empty()
            || !self.descriptor.scopes.is_subset(&claims.scopes)
        {
            bail!("ChatGPT token issuer, audience, account, or scope validation failed");
        }
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
        let expires_in = body
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(3600)
            .clamp(60, 24 * 60 * 60);
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
            expires_at: Utc::now() + TimeDelta::seconds(expires_in),
            generation: Uuid::new_v4().to_string(),
            revoked: false,
            mutation_pending: false,
        })
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
        if requested.scheme() != "https"
            || requested.host_str() != Some("chatgpt.com")
            || requested.port_or_known_default() != required.port_or_known_default()
            || !requested.path().starts_with("/backend-api/codex")
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

    #[derive(Clone)]
    struct FixedVerifier(VerifiedOpenAiClaims);

    impl OpenAiTokenVerifier for FixedVerifier {
        fn verify(
            &self,
            _id_token: Option<&str>,
            _access_token: &str,
        ) -> Result<VerifiedOpenAiClaims> {
            Ok(self.0.clone())
        }
    }

    fn claims() -> VerifiedOpenAiClaims {
        VerifiedOpenAiClaims {
            issuer: REQUIRED_TOKEN_ISSUER.into(),
            audiences: BTreeSet::from([OPENAI_PUBLIC_CLIENT_ID.into()]),
            subject: "subject-work".into(),
            account_id: "acct-work".into(),
            scopes: BTreeSet::from(["openid".into(), "offline_access".into()]),
            nonce: None,
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
            scopes: BTreeSet::from(["openid".into(), "offline_access".into()]),
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

    #[test]
    fn public_client_risk_is_versioned_and_verification_fails_closed() {
        let dialect = OpenAiChatGptOAuthDialect::production().unwrap();
        assert!(dialect
            .descriptor()
            .protocol_revision
            .contains("3e4707b34b16"));
        assert!(!dialect
            .descriptor()
            .allowed_origins
            .contains("https://api.openai.com"));
        let error = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "access_token": "secret-access",
                    "refresh_token": "secret-refresh",
                    "id_token": "secret-id",
                    "expires_in": 3600
                }),
                None,
                &TokenValidationContext::Device,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("verification is unavailable"));
        assert!(!error.contains("secret-"));
    }

    #[test]
    fn openai_dialect_exact_device_pkce_and_token_fixture_is_strictly_bound() {
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
        let pending = dialect
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
            )
            .unwrap();
        let metadata = tokens.provider_credential("chatgpt:work");
        assert_eq!(metadata.provider, CredentialProvider::ChatgptSubscription);
        assert_eq!(metadata.account.as_deref(), Some("acct-work"));
        assert_eq!(metadata.secret_ref, "oauth-store:chatgpt:work");
        assert!(!format!("{tokens:?}").contains("access-secret"));
    }

    #[test]
    fn openai_token_claim_mismatch_matrix_fails_before_record_creation() {
        for defect in ["issuer", "audience", "scope", "account", "nonce"] {
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
                    "scope" => claims.scopes.remove("offline_access"),
                    "account" => claims.account_id.clear(),
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
                )
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("validation failed") || error.contains("nonce mismatch"),
                "defect={defect} error={error}"
            );
            assert!(!error.contains("secret"));
        }
    }
}
