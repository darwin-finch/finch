//! Provider-neutral OAuth 2 authorization and credential lifecycle.
//!
//! The state machine in this module knows RFC 8628 and authorization-code
//! with PKCE mechanics, but deliberately knows no provider URL, client ID,
//! scope, token shape, or account claim. Those belong to [`OAuthDialect`].

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, TimeDelta, Utc};
use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::config::{
    AudienceBinding, CredentialKind, CredentialLifecycle, CredentialProvider, CredentialResolver,
    ProviderCredential, ResolvedCredential, ResolvedSecret,
};

const MAX_AUTH_BODY_BYTES: usize = 64 * 1024;
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);
const RFC8628_DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(not(test))]
const MIN_POLL_INTERVAL: Duration = RFC8628_DEFAULT_POLL_INTERVAL;
// Deterministic fake-server tests retain the same clamp path without waiting
// five wall-clock seconds between every scripted response.
#[cfg(test)]
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(not(test))]
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);
#[cfg(test)]
const SLOW_DOWN_STEP: Duration = Duration::from_millis(10);
const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FIELD_BYTES: usize = 4096;

/// Stable, provider-owned OAuth protocol description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthDialectDescriptor {
    pub dialect_id: String,
    pub protocol_revision: String,
    pub provider: CredentialProvider,
    pub credential_kind: CredentialKind,
    pub browser_credential_kind: Option<CredentialKind>,
    pub issuer: String,
    pub audience: AudienceBinding,
    pub client_id: String,
    pub scopes: BTreeSet<String>,
    pub device_authorization_endpoint: String,
    pub device_token_endpoint: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub revocation_endpoint: String,
    /// Exact origins to which this dialect permits credential-bearing POSTs.
    pub allowed_origins: BTreeSet<String>,
    /// Exact origins the user may be instructed to visit. These are separate
    /// from token endpoints because some providers use a distinct UI origin.
    pub allowed_user_authorization_origins: BTreeSet<String>,
    /// Test dialects may opt into loopback HTTP. Production dialects must not.
    pub allow_insecure_loopback: bool,
}

impl OAuthDialectDescriptor {
    /// Validate all static authority before any request is constructed.
    pub fn validate(&self) -> Result<()> {
        if self.dialect_id.trim().is_empty()
            || self.protocol_revision.trim().is_empty()
            || self.client_id.trim().is_empty()
            || self.issuer.trim().is_empty()
            || self.scopes.is_empty()
        {
            bail!("OAuth dialect descriptor is incomplete");
        }
        for endpoint in [
            &self.device_authorization_endpoint,
            &self.device_token_endpoint,
            &self.authorization_endpoint,
            &self.token_endpoint,
            &self.revocation_endpoint,
        ] {
            let url = validate_endpoint(endpoint, self.allow_insecure_loopback)?;
            let origin = origin(&url)?;
            if !self.allowed_origins.contains(&origin) {
                bail!("OAuth endpoint is outside its dialect's allowed origins");
            }
        }
        if self.allowed_user_authorization_origins.is_empty() {
            bail!("OAuth dialect has no allowed user authorization origin");
        }
        for allowed in &self.allowed_user_authorization_origins {
            let url = validate_endpoint(allowed, self.allow_insecure_loopback)?;
            if origin(&url)? != *allowed || url.path() != "/" || url.query().is_some() {
                bail!("OAuth user authorization authority must be a normalized origin");
            }
        }
        Ok(())
    }
}

/// An HTTP form request selected entirely by a provider dialect.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthHttpRequest {
    pub endpoint: String,
    pub body: OAuthRequestBody,
}

/// Audited request encoding chosen by a dialect.
#[derive(Clone, PartialEq, Eq)]
pub enum OAuthRequestBody {
    Form(Vec<(String, String)>),
    Json(Value),
}

impl fmt::Debug for OAuthRequestBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OAuthRequestBody([REDACTED])")
    }
}

impl fmt::Debug for OAuthHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthHttpRequest")
            .field("endpoint", &self.endpoint)
            .field("body", &"[REDACTED]")
            .finish()
    }
}

/// Result of an initial RFC 8628 device authorization request.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    #[zeroize(skip)]
    pub verification_uri: String,
    #[zeroize(skip)]
    pub verification_uri_complete: Option<String>,
    #[zeroize(skip)]
    pub expires_in: Duration,
    #[zeroize(skip)]
    pub interval: Duration,
}

impl fmt::Debug for DeviceAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceAuthorization")
            .field("device_code", &"[REDACTED]")
            .field("user_code", &"[REDACTED]")
            .field("verification_uri", &self.verification_uri)
            .field(
                "verification_uri_complete",
                &self
                    .verification_uri_complete
                    .as_ref()
                    .map(|_| "[REDACTED]"),
            )
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish()
    }
}

/// Provider interpretation of one device polling response.
pub enum DevicePoll {
    Pending,
    SlowDown,
    Denied,
    Expired,
    Tokens(Value),
    AuthorizationCode(AuthorizationCodeGrant),
}

/// A provider-issued authorization code plus the correlated verifier.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct AuthorizationCodeGrant {
    pub code: String,
    pub verifier: String,
    #[zeroize(skip)]
    pub redirect_uri: String,
}

impl fmt::Debug for AuthorizationCodeGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthorizationCodeGrant([REDACTED])")
    }
}

/// Pending browser authorization state. Debug never reveals correlation data.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct PendingBrowserAuthorization {
    #[zeroize(skip)]
    pub authorization_url: String,
    state: String,
    nonce: String,
    verifier: String,
    #[zeroize(skip)]
    redirect_uri: String,
    #[zeroize(skip)]
    expires_at: DateTime<Utc>,
}

impl fmt::Debug for PendingBrowserAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingBrowserAuthorization")
            .field("authorization_url", &"[REDACTED]")
            .field("state", &"[REDACTED]")
            .field("nonce", &"[REDACTED]")
            .field("verifier", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Provider-validated OAuth token record. Values are private and redacted.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct OAuthTokenRecord {
    pub dialect_id: String,
    pub protocol_revision: String,
    #[zeroize(skip)]
    pub provider: CredentialProvider,
    #[zeroize(skip)]
    pub kind: CredentialKind,
    pub issuer: String,
    #[zeroize(skip)]
    pub audience: AudienceBinding,
    pub client_id: String,
    pub account: String,
    #[zeroize(skip)]
    pub tenant: Option<String>,
    #[zeroize(skip)]
    pub project: Option<String>,
    #[zeroize(skip)]
    pub scopes: BTreeSet<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    #[zeroize(skip)]
    pub expires_at: DateTime<Utc>,
    pub generation: String,
    #[zeroize(skip)]
    pub revoked: bool,
    #[zeroize(skip)]
    pub mutation_pending: bool,
}

impl fmt::Debug for OAuthTokenRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthTokenRecord")
            .field("dialect_id", &self.dialect_id)
            .field("protocol_revision", &self.protocol_revision)
            .field("provider", &self.provider)
            .field("kind", &self.kind)
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("client_id", &"[REDACTED]")
            .field("account", &"[REDACTED]")
            .field("tenant", &self.tenant.as_ref().map(|_| "[REDACTED]"))
            .field("project", &self.project.as_ref().map(|_| "[REDACTED]"))
            .field("scopes", &self.scopes)
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "[REDACTED]"))
            .field("expires_at", &self.expires_at)
            .field("generation", &"[REDACTED]")
            .field("revoked", &self.revoked)
            .field("mutation_pending", &self.mutation_pending)
            .finish()
    }
}

impl OAuthTokenRecord {
    /// Construct #174 metadata without copying secret material into config.
    pub fn provider_credential(&self, name: &str) -> ProviderCredential {
        ProviderCredential {
            name: name.to_string(),
            kind: self.kind,
            provider: self.provider,
            issuer: self.issuer.clone(),
            audience: self.audience.clone(),
            tenant: self.tenant.clone(),
            project: self.project.clone(),
            account: Some(self.account.clone()),
            scopes: self.scopes.clone(),
            secret_ref: format!("oauth-store:{name}"),
            lifecycle: if self.revoked {
                CredentialLifecycle::Revoked
            } else {
                CredentialLifecycle::Active {
                    expires_at: Some(self.expires_at),
                    refreshable: self.refresh_token.is_some(),
                }
            },
            revocation: Default::default(),
        }
    }
}

/// Strict provider dialect boundary. Implementations own every provider fact.
pub trait OAuthDialect: Send + Sync {
    fn descriptor(&self) -> &OAuthDialectDescriptor;
    fn device_authorization_request(&self) -> Result<OAuthHttpRequest>;
    fn parse_device_authorization(
        &self,
        status: StatusCode,
        body: Value,
    ) -> Result<DeviceAuthorization>;
    fn device_poll_request(&self, pending: &DeviceAuthorization) -> Result<OAuthHttpRequest>;
    fn parse_device_poll(&self, status: StatusCode, body: Value) -> Result<DevicePoll>;
    fn authorization_code_request(
        &self,
        grant: &AuthorizationCodeGrant,
    ) -> Result<OAuthHttpRequest>;
    fn refresh_request(&self, refresh_token: &str) -> Result<OAuthHttpRequest>;
    fn revoke_request(&self, token: &str) -> Result<OAuthHttpRequest>;
    fn validate_tokens(
        &self,
        status: StatusCode,
        body: Value,
        previous: Option<&OAuthTokenRecord>,
        context: &TokenValidationContext,
    ) -> Result<OAuthTokenRecord>;
}

/// Correlation authority passed to provider-specific token validation.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub enum TokenValidationContext {
    Device,
    Browser {
        expected_nonce: String,
        redirect_uri: String,
    },
    Refresh,
}

impl fmt::Debug for TokenValidationContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device => formatter.write_str("TokenValidationContext::Device"),
            Self::Browser { .. } => {
                formatter.write_str("TokenValidationContext::Browser([REDACTED])")
            }
            Self::Refresh => formatter.write_str("TokenValidationContext::Refresh"),
        }
    }
}

/// Crash-safe token persistence boundary with generation compare-and-swap.
pub trait OAuthCredentialStore: Send + Sync {
    fn load(&self, reference: &str) -> Result<Option<OAuthTokenRecord>>;
    fn compare_and_swap(
        &self,
        reference: &str,
        expected_generation: Option<&str>,
        replacement: &OAuthTokenRecord,
    ) -> Result<()>;
}

/// Local #174 resolver backed by the same OAuth persistence used by login,
/// refresh, recovery, and logout. It never refreshes or contacts a provider.
pub struct StoredOAuthCredentialResolver<S> {
    store: Arc<S>,
    descriptor: OAuthDialectDescriptor,
}

impl<S> StoredOAuthCredentialResolver<S> {
    pub fn new(store: Arc<S>, descriptor: &OAuthDialectDescriptor) -> Result<Self> {
        descriptor.validate()?;
        Ok(Self {
            store,
            descriptor: descriptor.clone(),
        })
    }
}

impl<S> fmt::Debug for StoredOAuthCredentialResolver<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoredOAuthCredentialResolver([REDACTED STORE])")
    }
}

impl<S> CredentialResolver for StoredOAuthCredentialResolver<S>
where
    S: OAuthCredentialStore,
{
    fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential> {
        let reference = credential
            .secret_ref
            .strip_prefix("oauth-store:")
            .context("named OAuth credential has an incompatible secret reference")?;
        if reference != credential.name {
            bail!("named OAuth credential secret reference does not match its stable name");
        }
        let record = self
            .store
            .load(reference)?
            .context("named OAuth credential secret is missing; sign in explicitly")?;
        if !record_matches_descriptor(&record, &self.descriptor)
            || record.provider != credential.provider
            || record.kind != credential.kind
            || record.issuer != credential.issuer
            || record.audience != credential.audience
            || record.tenant != credential.tenant
            || record.project != credential.project
            || Some(record.account.as_str()) != credential.account.as_deref()
            || record.scopes != credential.scopes
            || record.revoked
            || record.mutation_pending
            || record.expires_at <= Utc::now()
        {
            bail!("named OAuth credential metadata or lifecycle does not match its stored token binding");
        }
        Ok(ResolvedCredential {
            credential_name: credential.name.clone(),
            secret: ResolvedSecret::new(record.access_token.clone())
                .context("named OAuth credential contains unusable secret material")?,
        })
    }
}

/// Provider-neutral OAuth state machine.
pub struct OAuthClient<D, S> {
    dialect: Arc<D>,
    store: Arc<S>,
    http: Client,
    timeout: Duration,
}

impl<D, S> fmt::Debug for OAuthClient<D, S>
where
    D: OAuthDialect,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthClient")
            .field("dialect", &self.dialect.descriptor().dialect_id)
            .field("store", &"[REDACTED CREDENTIAL STORE]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl<D, S> OAuthClient<D, S>
where
    D: OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    pub fn new(dialect: Arc<D>, store: Arc<S>) -> Result<Self> {
        dialect.descriptor().validate()?;
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("Failed to construct bounded OAuth HTTP client")?;
        Ok(Self {
            dialect,
            store,
            http,
            timeout: DEFAULT_HTTP_TIMEOUT,
        })
    }

    #[cfg(test)]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Start a device authorization without persisting transient codes.
    pub async fn begin_device_authorization(&self) -> Result<DeviceAuthorization> {
        let request = self.dialect.device_authorization_request()?;
        let (status, body) = self.post_form(request).await?;
        let pending = self
            .dialect
            .parse_device_authorization(status, body)
            .context("OAuth device authorization response is incompatible")?;
        validate_device_authorization(&pending, self.dialect.descriptor())?;
        Ok(pending)
    }

    /// Poll until terminal state, persist exactly one validated account, and
    /// return its #174 metadata.
    pub async fn finish_device_authorization(
        &self,
        reference: &str,
        pending: &DeviceAuthorization,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential> {
        validate_reference(reference)?;
        validate_device_authorization(pending, self.dialect.descriptor())?;
        let replacement_generation = self.reauthentication_generation(reference)?;
        let deadline = tokio::time::Instant::now() + pending.expires_in;
        let mut interval = bounded_poll_interval(pending.interval);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => bail!("OAuth device authorization was cancelled"),
                _ = tokio::time::sleep_until(deadline) => bail!("OAuth device authorization expired"),
                _ = tokio::time::sleep(interval) => {}
            }
            let request = self.dialect.device_poll_request(pending)?;
            let response = self.post_device_request(request, &cancel, deadline).await?;
            let tokens = match self.dialect.parse_device_poll(response.0, response.1)? {
                DevicePoll::Pending => continue,
                DevicePoll::SlowDown => {
                    interval = interval
                        .saturating_add(SLOW_DOWN_STEP)
                        .min(MAX_POLL_INTERVAL);
                    continue;
                }
                DevicePoll::Denied => bail!("OAuth device authorization was denied"),
                DevicePoll::Expired => bail!("OAuth device authorization expired"),
                DevicePoll::Tokens(body) => self.dialect.validate_tokens(
                    StatusCode::OK,
                    body,
                    None,
                    &TokenValidationContext::Device,
                )?,
                DevicePoll::AuthorizationCode(grant) => {
                    let request = self.dialect.authorization_code_request(&grant)?;
                    let (status, body) =
                        self.post_device_request(request, &cancel, deadline).await?;
                    self.dialect.validate_tokens(
                        status,
                        body,
                        None,
                        &TokenValidationContext::Device,
                    )?
                }
            };
            self.validate_record(&tokens, None)?;
            ensure_device_authorization_active(&cancel, deadline)?;
            self.store
                .compare_and_swap(reference, replacement_generation.as_deref(), &tokens)?;
            return Ok(tokens.provider_credential(reference));
        }
    }

    /// Begin browser authorization using state, nonce, and S256 PKCE.
    pub fn begin_browser_authorization(
        &self,
        redirect_uri: &str,
        lifetime: Duration,
    ) -> Result<PendingBrowserAuthorization> {
        let descriptor = self.dialect.descriptor();
        if descriptor.browser_credential_kind.is_none() {
            bail!("OAuth browser authorization is unsupported by this provider dialect revision");
        }
        let mut url = validate_endpoint(
            &descriptor.authorization_endpoint,
            descriptor.allow_insecure_loopback,
        )?;
        let redirect = Url::parse(redirect_uri).context("OAuth redirect URI is invalid")?;
        if redirect.scheme() != "http" || !redirect.host_str().is_some_and(is_loopback_host) {
            bail!("OAuth browser callback must use an exact loopback HTTP redirect");
        }
        let state = random_secret(32);
        let nonce = random_secret(32);
        let verifier = random_secret(48);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &descriptor.client_id)
            .append_pair("redirect_uri", redirect.as_str())
            .append_pair(
                "scope",
                &descriptor
                    .scopes
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" "),
            )
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(PendingBrowserAuthorization {
            authorization_url: url.to_string(),
            state,
            nonce,
            verifier,
            redirect_uri: redirect.to_string(),
            expires_at: Utc::now()
                + TimeDelta::from_std(lifetime).context("OAuth browser lifetime is invalid")?,
        })
    }

    /// Correlate an exact loopback callback, exchange its code, and persist it.
    pub async fn finish_browser_authorization(
        &self,
        reference: &str,
        pending: PendingBrowserAuthorization,
        callback_url: &str,
    ) -> Result<ProviderCredential> {
        validate_reference(reference)?;
        if Utc::now() >= pending.expires_at {
            bail!("OAuth browser authorization expired");
        }
        let replacement_generation = self.reauthentication_generation(reference)?;
        let callback = Url::parse(callback_url).context("OAuth callback URL is invalid")?;
        let expected = Url::parse(&pending.redirect_uri)?;
        if callback.scheme() != expected.scheme()
            || callback.host_str() != expected.host_str()
            || callback.port_or_known_default() != expected.port_or_known_default()
            || callback.path() != expected.path()
            || callback.fragment().is_some()
        {
            bail!("OAuth callback redirect does not match the pending authorization");
        }
        let params = callback.query_pairs().collect::<Vec<_>>();
        let one = |name: &str| -> Result<String> {
            let values = params
                .iter()
                .filter(|(key, _)| key == name)
                .map(|(_, value)| value.to_string())
                .collect::<Vec<_>>();
            if values.len() != 1 || values[0].is_empty() {
                bail!("OAuth callback has missing or duplicate correlation fields");
            }
            Ok(values[0].clone())
        };
        if one("state")? != pending.state {
            bail!("OAuth callback state mismatch");
        }
        let code = one("code")?;
        let grant = AuthorizationCodeGrant {
            code,
            verifier: pending.verifier.clone(),
            redirect_uri: pending.redirect_uri.clone(),
        };
        let request = self.dialect.authorization_code_request(&grant)?;
        let (status, body) = self.post_form(request).await?;
        let tokens = self.dialect.validate_tokens(
            status,
            body,
            None,
            &TokenValidationContext::Browser {
                expected_nonce: pending.nonce.clone(),
                redirect_uri: pending.redirect_uri.clone(),
            },
        )?;
        self.validate_record(&tokens, None)?;
        self.store
            .compare_and_swap(reference, replacement_generation.as_deref(), &tokens)?;
        Ok(tokens.provider_credential(reference))
    }

    /// Refresh with a crash marker and generation-checked token rotation.
    pub async fn refresh(&self, reference: &str) -> Result<ProviderCredential> {
        validate_reference(reference)?;
        let current = self
            .store
            .load(reference)?
            .context("named OAuth credential is missing")?;
        if current.mutation_pending {
            bail!("OAuth credential has an interrupted mutation; run credential recovery to tombstone it, then re-authenticate");
        }
        self.validate_refreshable_record(&current)?;
        let refresh = current
            .refresh_token
            .as_deref()
            .context("OAuth credential cannot be refreshed")?;
        let mut marker = current.clone();
        marker.generation = random_secret(24);
        marker.mutation_pending = true;
        self.store
            .compare_and_swap(reference, Some(&current.generation), &marker)?;
        let request = self.dialect.refresh_request(refresh)?;
        let (status, body) = self.post_form(request).await?;
        let refreshed = self.dialect.validate_tokens(
            status,
            body,
            Some(&current),
            &TokenValidationContext::Refresh,
        )?;
        self.validate_record(&refreshed, Some(&current))?;
        self.store
            .compare_and_swap(reference, Some(&marker.generation), &refreshed)
            .context("refreshed OAuth credential lost its local generation commit")?;
        Ok(refreshed.provider_credential(reference))
    }

    /// Revoke remotely, then retain a durable local tombstone.
    pub async fn revoke(&self, reference: &str) -> Result<ProviderCredential> {
        validate_reference(reference)?;
        let current = self
            .store
            .load(reference)?
            .context("named OAuth credential is missing")?;
        self.validate_mutable_record(&current)?;
        let token = current
            .refresh_token
            .as_deref()
            .unwrap_or(&current.access_token);
        let request = self.dialect.revoke_request(token)?;
        let mut marker = current.clone();
        marker.generation = random_secret(24);
        marker.mutation_pending = true;
        self.store
            .compare_and_swap(reference, Some(&current.generation), &marker)?;
        let (status, _) = self.post_form(request).await?;
        if !status.is_success() {
            bail!("OAuth provider rejected credential revocation");
        }
        let mut tombstone = current.clone();
        tombstone.access_token.clear();
        tombstone.refresh_token = None;
        tombstone.id_token = None;
        tombstone.generation = random_secret(24);
        tombstone.revoked = true;
        tombstone.mutation_pending = false;
        self.store
            .compare_and_swap(reference, Some(&marker.generation), &tombstone)?;
        Ok(tombstone.provider_credential(reference))
    }

    /// Resolve a crash-interrupted mutation without trusting or transmitting
    /// its possibly rotated tokens. The durable tombstone permits an explicit
    /// fresh login while preventing stale resurrection.
    pub fn recover_interrupted_as_revoked(&self, reference: &str) -> Result<ProviderCredential> {
        validate_reference(reference)?;
        let current = self
            .store
            .load(reference)?
            .context("named OAuth credential is missing")?;
        self.validate_record_binding(&current)?;
        if !current.mutation_pending {
            bail!("OAuth credential has no interrupted mutation to recover");
        }
        let mut tombstone = current.clone();
        tombstone.access_token.clear();
        tombstone.refresh_token = None;
        tombstone.id_token = None;
        tombstone.generation = random_secret(24);
        tombstone.revoked = true;
        tombstone.mutation_pending = false;
        self.store
            .compare_and_swap(reference, Some(&current.generation), &tombstone)?;
        Ok(tombstone.provider_credential(reference))
    }

    fn validate_record(
        &self,
        record: &OAuthTokenRecord,
        previous: Option<&OAuthTokenRecord>,
    ) -> Result<()> {
        self.validate_record_binding(record)?;
        if !self.dialect.descriptor().scopes.is_subset(&record.scopes) {
            bail!("OAuth token binding does not match the selected provider dialect");
        }
        if record.account.trim().is_empty()
            || record.access_token.trim().is_empty()
            || record.generation.trim().is_empty()
            || record.expires_at <= Utc::now()
            || record.revoked
            || record.mutation_pending
        {
            bail!("OAuth token lifecycle or account binding is unusable");
        }
        if let Some(previous) = previous {
            if previous.account != record.account
                || previous.tenant != record.tenant
                || previous.project != record.project
            {
                bail!("OAuth refresh changed the bound account identity");
            }
        }
        Ok(())
    }

    fn validate_refreshable_record(&self, record: &OAuthTokenRecord) -> Result<()> {
        self.validate_mutable_record(record)?;
        if record.refresh_token.as_deref().is_none_or(str::is_empty) {
            bail!("OAuth credential cannot be refreshed");
        }
        Ok(())
    }

    fn validate_mutable_record(&self, record: &OAuthTokenRecord) -> Result<()> {
        self.validate_record_binding(record)?;
        if record.account.trim().is_empty()
            || record.access_token.trim().is_empty()
            || record.generation.trim().is_empty()
            || record.revoked
            || record.mutation_pending
        {
            bail!("OAuth token lifecycle or account binding is unusable");
        }
        Ok(())
    }

    fn reauthentication_generation(&self, reference: &str) -> Result<Option<String>> {
        let Some(record) = self.store.load(reference)? else {
            return Ok(None);
        };
        self.validate_record_binding(&record)?;
        if !record.revoked || record.mutation_pending || record.generation.trim().is_empty() {
            bail!("named OAuth credential already exists; revoke it before explicit re-authentication");
        }
        Ok(Some(record.generation.clone()))
    }

    fn validate_record_binding(&self, record: &OAuthTokenRecord) -> Result<()> {
        if !record_matches_descriptor(record, self.dialect.descriptor()) {
            bail!("OAuth token binding does not match the selected provider dialect");
        }
        Ok(())
    }

    async fn post_form(&self, request: OAuthHttpRequest) -> Result<(StatusCode, Value)> {
        let descriptor = self.dialect.descriptor();
        let url = validate_endpoint(&request.endpoint, descriptor.allow_insecure_loopback)?;
        if !descriptor.allowed_origins.contains(&origin(&url)?) {
            bail!("OAuth request attempted endpoint substitution");
        }
        let builder = self.http.post(url);
        let builder = match request.body {
            OAuthRequestBody::Form(fields) => builder.form(&fields),
            OAuthRequestBody::Json(value) => builder.json(&value),
        };
        tokio::time::timeout(self.timeout, async {
            let response = builder.send().await.context("OAuth request failed")?;
            if response.status().is_redirection() {
                bail!("OAuth endpoint redirect was rejected");
            }
            let status = response.status();
            let bytes = read_bounded(response, MAX_AUTH_BODY_BYTES).await?;
            let body =
                serde_json::from_slice(&bytes).context("OAuth response was malformed JSON")?;
            Ok((status, body))
        })
        .await
        .context("OAuth request timed out")?
    }

    async fn post_device_request(
        &self,
        request: OAuthHttpRequest,
        cancel: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Result<(StatusCode, Value)> {
        ensure_device_authorization_active(cancel, deadline)?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("OAuth device authorization was cancelled"),
            _ = tokio::time::sleep_until(deadline) => bail!("OAuth device authorization expired"),
            response = self.post_form(request) => response,
        }
    }
}

fn ensure_device_authorization_active(
    cancel: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<()> {
    if cancel.is_cancelled() {
        bail!("OAuth device authorization was cancelled");
    }
    if tokio::time::Instant::now() >= deadline {
        bail!("OAuth device authorization expired");
    }
    Ok(())
}

fn record_matches_descriptor(
    record: &OAuthTokenRecord,
    descriptor: &OAuthDialectDescriptor,
) -> bool {
    record.dialect_id == descriptor.dialect_id
        && record.protocol_revision == descriptor.protocol_revision
        && record.provider == descriptor.provider
        && (record.kind == descriptor.credential_kind
            || Some(record.kind) == descriptor.browser_credential_kind)
        && record.issuer == descriptor.issuer
        && record.audience == descriptor.audience
        && record.client_id == descriptor.client_id
        && descriptor.scopes.is_subset(&record.scopes)
}

fn bounded_poll_interval(advertised: Duration) -> Duration {
    advertised.clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)
}

async fn read_bounded(response: reqwest::Response, maximum: usize) -> Result<Vec<u8>> {
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("OAuth response body failed")?;
        if body.len().saturating_add(chunk.len()) > maximum {
            bail!("OAuth response exceeded the size limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn validate_device_authorization(
    pending: &DeviceAuthorization,
    descriptor: &OAuthDialectDescriptor,
) -> Result<()> {
    validate_secret_field(&pending.device_code, "device code")?;
    validate_secret_field(&pending.user_code, "user code")?;
    let verification = validate_endpoint(
        &pending.verification_uri,
        descriptor.allow_insecure_loopback,
    )?;
    if !descriptor
        .allowed_user_authorization_origins
        .contains(&origin(&verification)?)
    {
        bail!(
            "OAuth device verification URI is outside the provider dialect's allowed user origin"
        );
    }
    if let Some(url) = &pending.verification_uri_complete {
        let complete = validate_endpoint(url, descriptor.allow_insecure_loopback)?;
        if !descriptor
            .allowed_user_authorization_origins
            .contains(&origin(&complete)?)
        {
            bail!("OAuth complete verification URI is outside the provider dialect's allowed user origin");
        }
    }
    if pending.expires_in.is_zero() || pending.expires_in > Duration::from_secs(30 * 60) {
        bail!("OAuth device authorization expiry is invalid");
    }
    if pending.interval > MAX_POLL_INTERVAL {
        bail!("OAuth device polling interval is invalid");
    }
    Ok(())
}

fn validate_reference(reference: &str) -> Result<()> {
    if reference.is_empty()
        || reference.len() > 128
        || !reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
    {
        bail!("invalid named OAuth credential reference");
    }
    Ok(())
}

pub(crate) fn validate_secret_field(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_FIELD_BYTES || value.chars().any(char::is_control) {
        bail!("OAuth {label} is invalid");
    }
    Ok(())
}

fn validate_endpoint(value: &str, allow_insecure_loopback: bool) -> Result<Url> {
    let url = Url::parse(value).context("OAuth endpoint must be an absolute URL")?;
    if url.username() != "" || url.password().is_some() || url.fragment().is_some() {
        bail!("OAuth endpoint contains forbidden authority or fragment data");
    }
    let secure = url.scheme() == "https";
    let test_loopback = allow_insecure_loopback
        && url.scheme() == "http"
        && url.host_str().is_some_and(is_loopback_host);
    if !secure && !test_loopback {
        bail!("OAuth endpoints require TLS");
    }
    Ok(url)
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn origin(url: &Url) -> Result<String> {
    let host = url.host_str().context("OAuth endpoint has no host")?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_ascii_lowercase()
    };
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Ok(format!("{}://{}{}", url.scheme(), host, port))
}

fn random_secret(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut value);
    let encoded = URL_SAFE_NO_PAD.encode(&value);
    value.zeroize();
    encoded
}

pub mod file_store;

#[cfg(test)]
mod tests;
