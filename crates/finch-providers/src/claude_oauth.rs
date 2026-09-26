//! Anthropic Claude subscription OAuth (authorization-code + PKCE) dialect.
//!
//! This adapter reproduces the browser sign-in protocol used by Anthropic's
//! own Claude Code CLI to authenticate a claude.ai Pro/Max/Team subscription
//! (as opposed to a Console API key). Unlike the ChatGPT/Grok dialects, this
//! is **moderate-confidence, third-party reverse-engineered** protocol
//! detail, not an audited pin of a public open-source revision:
//!
//! - Client ID, endpoints, and the `CLAUDE_AI_OAUTH_SCOPES` scope set were
//!   corroborated across independent reverse-engineering write-ups and an
//!   apparent extraction of Claude Code's own `oauth.ts`/`client.ts` source
//!   (constant names, internal `USER_TYPE === 'ant'`/staging-only branches,
//!   and `tengu_oauth_*` analytics event names strongly suggest this is
//!   genuine, not a fabricated guess), but none of this is first-party
//!   published documentation.
//! - The scope set requested here (`CLAUDE_AI_OAUTH_SCOPES`) is a
//!   **deliberate subset** of what Claude Code itself requests at login. Its
//!   own source requests the union of `CONSOLE_OAUTH_SCOPES`
//!   (`org:create_api_key`, `user:profile` — Console API-key creation) and
//!   `CLAUDE_AI_OAUTH_SCOPES` (this dialect's set) in one authorize call,
//!   because one login button serves both account types. Finch requests only
//!   the Claude.ai/subscription subset: granting `org:create_api_key` would
//!   let a credential typed as a subscription credential mint a Console API
//!   key, which is exactly the cross-billing conflation this crate's
//!   "subscription and API billing are never automatically interchangeable"
//!   invariant forbids. This narrower request is proven to work at
//!   refresh-token time in the extracted source (it is literally the
//!   fallback scope value there) but has not been separately confirmed
//!   against a live `/authorize` leg; see the crate/PR notes for what a real
//!   login attempt would need to confirm.
//! - No source (including the extracted client) references any Anthropic
//!   OAuth *revocation* endpoint for this client id, unlike ChatGPT and
//!   Grok's dialects which both have one. `revoke_request` below fails
//!   closed rather than guessing a URL and silently mis-reporting success;
//!   callers fall back to a local-only tombstone (`src/cli/claude_auth.rs`).
//!
//! The compatibility revision is a fail-closed fence, not a claim of a
//! stable third-party contract.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::oauth::{
    validate_secret_field, AuthorizationCodeGrant, DeviceAuthorization, DevicePoll, OAuthDialect,
    OAuthDialectDescriptor, OAuthHttpRequest, OAuthRequestBody, OAuthTokenRecord,
    TokenValidationContext,
};
use crate::{AudienceBinding, CredentialKind, CredentialProvider, EndpointFamily};

pub const CLAUDE_OAUTH_PROTOCOL_REVISION: &str =
    "anthropic-claude-code-oauth@2026-09-25-reverse-engineered+finch-binding-v1";
/// Anthropic's own Claude Code application client id. This is reused, not
/// registered by Finch; see the crate/README notes on the ToS consideration
/// this implies. That call was made deliberately and is not re-litigated
/// here.
pub const CLAUDE_SUBSCRIPTION_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub(crate) const CLAUDE_AUTH_ORIGIN: &str = "https://claude.ai";
/// Current token-endpoint origin. Older reverse-engineering write-ups (and
/// this feature's original brief) name `console.anthropic.com`; the more
/// recent extracted client source uses `platform.claude.com` throughout
/// (Anthropic appears to have migrated Console branding). Both are plausible
/// for a still-working alias; this dialect pins the more recently observed
/// value and documents the older one here for a fast pivot if live testing
/// shows otherwise.
pub(crate) const CLAUDE_TOKEN_ORIGIN: &str = "https://platform.claude.com";
pub const CLAUDE_AUTHORIZATION_ENDPOINT: &str = "https://claude.ai/oauth/authorize";
pub const CLAUDE_TOKEN_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";
/// Anthropic's OAuth beta header. Claude Code sends this on every OAuth token
/// call, and inference requests authenticated with an OAuth bearer token
/// (rather than an `x-api-key`) also carry it; see `claude_subscription.rs`.
pub const CLAUDE_OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";

/// Finch-local capability requested for a Claude subscription credential.
///
/// This is `CLAUDE_AI_OAUTH_SCOPES` from the extracted Claude Code source,
/// deliberately excluding `org:create_api_key` (Console API-key creation —
/// see the module doc comment). Token response `scope` fields are not proof
/// of requested authority and are not projected into Finch's credential
/// graph; `validate_tokens` below always records this exact requested set.
pub fn claude_required_scopes() -> BTreeSet<String> {
    BTreeSet::from([
        "user:profile".into(),
        "user:inference".into(),
        "user:sessions:claude_code".into(),
        "user:mcp_servers".into(),
        "user:file_upload".into(),
    ])
}

/// Secret-free stage markers for actionable login diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClaudeAuthStageError {
    #[error("Claude subscription does not support device-code authorization; use browser sign-in")]
    DeviceFlowUnsupported,
    #[error("Claude subscription authorization-code exchange was rejected (HTTP {0})")]
    TokenExchangeRejected(u16),
    #[error("Claude subscription authorization-code exchange response changed")]
    TokenExchangeContract,
    #[error("Claude subscription account identity is missing or invalid")]
    AccountEntitlement,
    #[error(
        "Anthropic OAuth exposes no known public token-revocation endpoint for this client; Finch can only forget this credential locally"
    )]
    RevocationUnsupported,
}

/// Strict Anthropic-specific dialect; reusable OAuth state remains in `oauth`.
///
/// Unlike the ChatGPT/Grok dialects, this one has no injected JWKS verifier:
/// Anthropic's token response carries account identity directly as JSON
/// (`account.uuid`/`account.email_address`, `organization.uuid`) rather than
/// a signed `id_token` requiring signature verification.
pub struct ClaudeOAuthDialect {
    descriptor: OAuthDialectDescriptor,
}

impl ClaudeOAuthDialect {
    pub fn production() -> Result<Self> {
        Self::new(CLAUDE_AUTH_ORIGIN, CLAUDE_TOKEN_ORIGIN, false)
    }

    fn new(auth_origin: &str, token_origin: &str, allow_insecure_loopback: bool) -> Result<Self> {
        let auth_origin = auth_origin.trim_end_matches('/').to_string();
        let token_origin = token_origin.trim_end_matches('/').to_string();
        let allowed_origins = BTreeSet::from([auth_origin.clone(), token_origin.clone()]);
        let descriptor = OAuthDialectDescriptor {
            dialect_id: "anthropic_claude_subscription".into(),
            protocol_revision: CLAUDE_OAUTH_PROTOCOL_REVISION.into(),
            provider: CredentialProvider::ClaudeSubscription,
            credential_kind: CredentialKind::OauthBrowserPkce,
            browser_credential_kind: Some(CredentialKind::OauthBrowserPkce),
            issuer: "anthropic-claude".into(),
            audience: AudienceBinding::standard(EndpointFamily::ClaudeSubscription),
            client_id: CLAUDE_SUBSCRIPTION_CLIENT_ID.into(),
            scopes: claude_required_scopes(),
            // Claude subscription never uses device-code authorization; these
            // two fields exist only so the shared descriptor's non-empty-URL
            // contract validates. `device_authorization_request`/
            // `device_poll_request` below fail before ever building a request
            // against them.
            device_authorization_endpoint: format!("{token_origin}/oauth/device_unsupported"),
            device_token_endpoint: format!("{token_origin}/oauth/device_token_unsupported"),
            authorization_endpoint: format!("{auth_origin}/oauth/authorize"),
            token_endpoint: format!("{token_origin}/v1/oauth/token"),
            // Not used: `revoke_request` always fails (see the module doc
            // comment) because no available source establishes that
            // Anthropic exposes a public revocation endpoint for this
            // client. This field exists only to satisfy the descriptor's
            // required-URL contract.
            revocation_endpoint: format!("{token_origin}/v1/oauth/revoke"),
            allowed_origins,
            allowed_user_authorization_origins: BTreeSet::from([auth_origin]),
            allow_insecure_loopback,
        };
        descriptor.validate()?;
        Ok(Self { descriptor })
    }

    pub fn for_test(auth_origin: &str, token_origin: &str) -> Result<Self> {
        Self::new(auth_origin, token_origin, true)
    }
}

#[async_trait]
impl OAuthDialect for ClaudeOAuthDialect {
    fn descriptor(&self) -> &OAuthDialectDescriptor {
        &self.descriptor
    }

    fn device_authorization_request(&self) -> Result<OAuthHttpRequest> {
        Err(ClaudeAuthStageError::DeviceFlowUnsupported.into())
    }

    fn parse_device_authorization(
        &self,
        _status: StatusCode,
        _body: Value,
    ) -> Result<DeviceAuthorization> {
        Err(ClaudeAuthStageError::DeviceFlowUnsupported.into())
    }

    fn device_poll_request(&self, _pending: &DeviceAuthorization) -> Result<OAuthHttpRequest> {
        Err(ClaudeAuthStageError::DeviceFlowUnsupported.into())
    }

    fn parse_device_poll(&self, _status: StatusCode, _body: Value) -> Result<DevicePoll> {
        Err(ClaudeAuthStageError::DeviceFlowUnsupported.into())
    }

    fn authorization_code_request(
        &self,
        grant: &AuthorizationCodeGrant,
    ) -> Result<OAuthHttpRequest> {
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.token_endpoint.clone(),
            body: OAuthRequestBody::Json(json!({
                "grant_type": "authorization_code",
                "code": grant.code,
                "redirect_uri": grant.redirect_uri,
                "client_id": self.descriptor.client_id,
                "code_verifier": grant.verifier,
            })),
        })
    }

    fn refresh_request(&self, refresh_token: &str) -> Result<OAuthHttpRequest> {
        validate_secret_field(refresh_token, "refresh token")?;
        Ok(OAuthHttpRequest {
            endpoint: self.descriptor.token_endpoint.clone(),
            body: OAuthRequestBody::Json(json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": self.descriptor.client_id,
                "scope": scope_string(&self.descriptor.scopes),
            })),
        })
    }

    fn revoke_request(&self, _token: &str) -> Result<OAuthHttpRequest> {
        Err(ClaudeAuthStageError::RevocationUnsupported.into())
    }

    async fn validate_tokens(
        &self,
        status: StatusCode,
        body: Value,
        previous: Option<&OAuthTokenRecord>,
        context: &TokenValidationContext,
        _cancel: &CancellationToken,
    ) -> Result<OAuthTokenRecord> {
        if !status.is_success() {
            return Err(ClaudeAuthStageError::TokenExchangeRejected(status.as_u16()).into());
        }
        if matches!(context, TokenValidationContext::Refresh) && previous.is_none() {
            bail!("Claude subscription refresh was requested without a prior bound record");
        }
        let access_token = required_string(&body, "access_token")
            .context(ClaudeAuthStageError::TokenExchangeContract)?;
        let refresh_token = body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| previous.and_then(|record| record.refresh_token.clone()))
            .context(ClaudeAuthStageError::TokenExchangeContract)?;
        validate_secret_field(&refresh_token, "refresh token")
            .context(ClaudeAuthStageError::TokenExchangeContract)?;
        let expires_in = body
            .get("expires_in")
            .and_then(Value::as_u64)
            .context(ClaudeAuthStageError::TokenExchangeContract)?;
        // Anthropic's own token TTL is unconfirmed by any first-party source;
        // one reverse-engineering write-up states ~28800s (8h). Bound to a
        // generous but finite range rather than trusting an unbounded value.
        if expires_in == 0 || expires_in > 30 * 24 * 60 * 60 {
            return Err(ClaudeAuthStageError::TokenExchangeContract.into());
        }
        // `expires_in` is already bounded above to at most 30 days in
        // seconds, well inside `TimeDelta::seconds`'s non-panicking range.
        let expires_at = Utc::now() + TimeDelta::seconds(expires_in as i64);
        let account = extract_account(&body)?
            .or_else(|| previous.map(|record| record.account.clone()))
            .context(ClaudeAuthStageError::AccountEntitlement)?;
        validate_public_claim(&account, "account identifier")
            .context(ClaudeAuthStageError::AccountEntitlement)?;
        let tenant = body
            .get("organization")
            .and_then(Value::as_object)
            .and_then(|organization| organization.get("uuid"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| previous.and_then(|record| record.tenant.clone()));
        if let Some(tenant) = tenant.as_deref() {
            validate_public_claim(tenant, "organization identifier")
                .context(ClaudeAuthStageError::AccountEntitlement)?;
        }
        if let Some(previous) = previous {
            if previous.account != account {
                return Err(ClaudeAuthStageError::AccountEntitlement.into());
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
            account,
            tenant,
            project: None,
            scopes: self.descriptor.scopes.clone(),
            access_token,
            refresh_token: Some(refresh_token),
            id_token: None,
            expires_at,
            generation: Uuid::new_v4().to_string(),
            revoked: false,
            mutation_pending: false,
        })
    }
}

fn scope_string(scopes: &BTreeSet<String>) -> String {
    scopes.iter().cloned().collect::<Vec<_>>().join(" ")
}

fn extract_account(body: &Value) -> Result<Option<String>> {
    let Some(account) = body.get("account").and_then(Value::as_object) else {
        return Ok(None);
    };
    let uuid = account.get("uuid").and_then(Value::as_str);
    let email = account.get("email_address").and_then(Value::as_str);
    match uuid.or(email) {
        Some(value) if !value.trim().is_empty() => Ok(Some(value.to_string())),
        _ => Ok(None),
    }
}

fn validate_public_claim(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("Claude signed {label} is invalid");
    }
    Ok(())
}

fn required_string(body: &Value, field: &str) -> Result<String> {
    let value = body
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("Claude subscription response omitted {field}"))?
        .to_string();
    validate_secret_field(&value, field)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::OAuthClient;
    use std::sync::Arc;

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

    fn browser_context() -> TokenValidationContext {
        TokenValidationContext::Browser {
            expected_nonce: "nonce-does-not-apply-to-this-dialect".into(),
            redirect_uri: "http://127.0.0.1:12345/callback".into(),
        }
    }

    #[tokio::test]
    async fn production_dialect_targets_the_pinned_claude_ai_and_platform_origins() {
        let dialect = ClaudeOAuthDialect::production().unwrap();
        assert_eq!(
            dialect.descriptor().authorization_endpoint,
            "https://claude.ai/oauth/authorize"
        );
        assert_eq!(
            dialect.descriptor().token_endpoint,
            "https://platform.claude.com/v1/oauth/token"
        );
        assert_eq!(
            dialect.descriptor().client_id,
            "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
        );
        assert_eq!(
            dialect.descriptor().browser_credential_kind,
            Some(CredentialKind::OauthBrowserPkce)
        );
        assert_eq!(
            dialect.descriptor().scopes,
            BTreeSet::from([
                "user:profile".to_string(),
                "user:inference".to_string(),
                "user:sessions:claude_code".to_string(),
                "user:mcp_servers".to_string(),
                "user:file_upload".to_string(),
            ]),
            "must request the Claude.ai subscription scope subset, not org:create_api_key"
        );
        assert!(!dialect.descriptor().scopes.contains("org:create_api_key"));
        OAuthClient::new(Arc::new(dialect), Arc::new(NoopStore)).unwrap();
    }

    #[test]
    fn device_flow_is_refused_before_any_request_is_built() {
        let dialect =
            ClaudeOAuthDialect::for_test("http://127.0.0.1:1", "http://127.0.0.1:1").unwrap();
        let error = dialect.device_authorization_request().unwrap_err();
        assert_eq!(
            error.downcast_ref::<ClaudeAuthStageError>(),
            Some(&ClaudeAuthStageError::DeviceFlowUnsupported)
        );
    }

    #[test]
    fn revoke_request_fails_closed_instead_of_guessing_an_endpoint() {
        let dialect =
            ClaudeOAuthDialect::for_test("http://127.0.0.1:1", "http://127.0.0.1:1").unwrap();
        let error = dialect.revoke_request("token").unwrap_err();
        assert_eq!(
            error.downcast_ref::<ClaudeAuthStageError>(),
            Some(&ClaudeAuthStageError::RevocationUnsupported)
        );
    }

    #[tokio::test]
    async fn token_exchange_binds_account_and_organization_without_an_id_token() {
        let dialect =
            ClaudeOAuthDialect::for_test("http://127.0.0.1:1", "http://127.0.0.1:1").unwrap();
        let record = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "access_token": "access-secret",
                    "refresh_token": "refresh-secret",
                    "expires_in": 28800,
                    "scope": "user:profile user:inference",
                    "account": {"uuid": "acct-uuid", "email_address": "user@example.com"},
                    "organization": {"uuid": "org-uuid"}
                }),
                None,
                &browser_context(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(record.account, "acct-uuid");
        assert_eq!(record.tenant.as_deref(), Some("org-uuid"));
        assert_eq!(record.id_token, None);
        // Requested authority is recorded, not whatever the response echoes.
        assert_eq!(record.scopes, claude_required_scopes());
        let metadata = record.provider_credential("claude:work");
        assert_eq!(metadata.provider, CredentialProvider::ClaudeSubscription);
        assert!(!format!("{record:?}").contains("access-secret"));
    }

    #[tokio::test]
    async fn refresh_without_a_fresh_account_reuses_the_bound_account_and_rejects_a_swap() {
        let dialect =
            ClaudeOAuthDialect::for_test("http://127.0.0.1:1", "http://127.0.0.1:1").unwrap();
        let initial = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "access_token": "access-1",
                    "refresh_token": "refresh-1",
                    "expires_in": 28800,
                    "account": {"uuid": "acct-uuid"}
                }),
                None,
                &browser_context(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        // A refresh response that omits `account` reuses the bound identity.
        let refreshed = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "access_token": "access-2",
                    "expires_in": 28800
                }),
                Some(&initial),
                &TokenValidationContext::Refresh,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(refreshed.account, "acct-uuid");
        assert_eq!(refreshed.refresh_token.as_deref(), Some("refresh-1"));

        // A refresh response naming a DIFFERENT account must be rejected, not
        // silently rebind the local credential to another Anthropic account.
        let error = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "access_token": "access-3",
                    "expires_in": 28800,
                    "account": {"uuid": "acct-other"}
                }),
                Some(&initial),
                &TokenValidationContext::Refresh,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ClaudeAuthStageError>(),
            Some(&ClaudeAuthStageError::AccountEntitlement)
        );
    }

    #[tokio::test]
    async fn hostile_or_missing_token_fields_are_rejected_without_leaking_secrets() {
        let dialect =
            ClaudeOAuthDialect::for_test("http://127.0.0.1:1", "http://127.0.0.1:1").unwrap();
        for (case, body) in [
            (
                "missing_access_token",
                json!({"expires_in": 3600, "account": {"uuid": "a"}}),
            ),
            (
                "missing_expiry",
                json!({"access_token": "a", "account": {"uuid": "a"}}),
            ),
            (
                "zero_expiry",
                json!({"access_token": "a", "expires_in": 0, "account": {"uuid": "a"}}),
            ),
            (
                "missing_account",
                json!({"access_token": "a", "expires_in": 3600}),
            ),
            (
                "control_char_account",
                json!({"access_token": "a", "expires_in": 3600, "account": {"uuid": "a\nforged"}}),
            ),
        ] {
            let error = dialect
                .validate_tokens(
                    StatusCode::OK,
                    body,
                    None,
                    &browser_context(),
                    &CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert!(
                error.downcast_ref::<ClaudeAuthStageError>().is_some(),
                "{case} did not fail with a typed stage error: {error}"
            );
        }

        let rejected = dialect
            .validate_tokens(
                StatusCode::BAD_REQUEST,
                json!({"error": "invalid_grant", "secret_sentinel": "reflect-me"}),
                None,
                &browser_context(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            rejected.downcast_ref::<ClaudeAuthStageError>(),
            Some(&ClaudeAuthStageError::TokenExchangeRejected(400))
        );
        assert!(!rejected.to_string().contains("reflect-me"));
    }

    #[test]
    fn authorization_and_refresh_requests_use_json_bodies_and_never_log_secrets() {
        let dialect =
            ClaudeOAuthDialect::for_test("http://127.0.0.1:1", "http://127.0.0.1:1").unwrap();
        let grant = AuthorizationCodeGrant {
            code: "auth-code-secret".into(),
            verifier: "verifier-secret".into(),
            redirect_uri: "http://127.0.0.1:12345/callback".into(),
        };
        let request = dialect.authorization_code_request(&grant).unwrap();
        assert_eq!(request.endpoint, dialect.descriptor().token_endpoint);
        assert!(matches!(request.body, OAuthRequestBody::Json(_)));
        assert!(!format!("{request:?}").contains("auth-code-secret"));
        assert!(!format!("{request:?}").contains("verifier-secret"));

        let refresh = dialect.refresh_request("refresh-secret").unwrap();
        assert_eq!(refresh.endpoint, dialect.descriptor().token_endpoint);
        assert!(!format!("{refresh:?}").contains("refresh-secret"));
    }
}
