//! Finch-native Claude subscription transport.
//!
//! Unlike the ChatGPT subscription adapter (a distinct Responses-Lite wire
//! protocol), Claude subscription inference uses the *same* Anthropic
//! Messages API shape as the API-key `claude` provider — only the
//! authentication mechanism differs (OAuth bearer token + `anthropic-beta`
//! header instead of a static `x-api-key`). This module therefore owns only
//! credential leasing/refresh and reuses `claude::ClaudeProvider` as its wire
//! transport, mirroring how `grok_subscription.rs` reuses `openai::OpenAIProvider`.
//!
//! Credentials, origin, and errors are not interchangeable with the
//! API-key `Anthropic`/`claude` adapter: a subscription lease is never
//! presented as, or silently substituted for, a Console API key.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use std::fmt;
use std::sync::{Arc, OnceLock, Weak};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::claude::ClaudeProvider;
use super::claude_oauth::ClaudeOAuthDialect;
#[cfg(test)]
use super::ProviderRequest;
use super::{
    CapabilitySupport, LlmProvider, ModelCapabilities, ProviderBackend, ProviderResponse,
    ReasoningCapability, StreamChunk, ValidatedProviderRequest, WireProtocol,
};
use crate::oauth::{FileOAuthCredentialStore, OAuthClient, OAuthCredentialStore, OAuthTokenRecord};
use crate::{
    AudienceBinding, CredentialProvider, EndpointFamily, ProviderCredential, ReasoningEffort,
};

/// Same Anthropic Messages API origin the API-key `claude` provider uses.
/// Subscription and API-key credentials remain distinct `CredentialProvider`
/// values even though inference is presented to the same host.
const CLAUDE_SUBSCRIPTION_BASE_URL: &str = crate::claude::CLAUDE_API_BASE_URL;
const REFRESH_SKEW: ChronoDuration = ChronoDuration::minutes(2);

#[derive(Debug)]
struct SubscriptionUnauthorized;

impl fmt::Display for SubscriptionUnauthorized {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Claude subscription authorization was rejected")
    }
}

impl std::error::Error for SubscriptionUnauthorized {}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct ClaudeCredentialLease {
    access_token: String,
    account: String,
    generation: String,
}

impl fmt::Debug for ClaudeCredentialLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ClaudeCredentialLease([REDACTED])")
    }
}

#[async_trait]
pub(crate) trait ClaudeCredentialSource: Send + Sync {
    async fn lease(&self, cancel: &CancellationToken) -> Result<ClaudeCredentialLease>;
    async fn refresh_after_unauthorized(
        &self,
        rejected_generation: &str,
        cancel: &CancellationToken,
    ) -> Result<ClaudeCredentialLease>;
}

type ProductionOAuthClient = OAuthClient<ClaudeOAuthDialect, FileOAuthCredentialStore>;

struct ProductionCredentialSource {
    reference: String,
    expected_account: String,
    store: Arc<FileOAuthCredentialStore>,
    oauth: Arc<ProductionOAuthClient>,
    refresh_lock: Arc<Mutex<()>>,
}

fn shared_refresh_lock(reference: &str, account: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<std::collections::HashMap<String, Weak<Mutex<()>>>>> =
        OnceLock::new();
    let key = format!("{reference}\0{account}");
    let mut locks = LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, lock| lock.strong_count() != 0);
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

impl ProductionCredentialSource {
    fn new(credential: &ProviderCredential) -> Result<Self> {
        let root = dirs::home_dir()
            .context("Could not determine Finch credential store location")?
            .join(".finch")
            .join("oauth");
        Self::new_in_root(credential, root)
    }

    fn new_in_root(
        credential: &ProviderCredential,
        root: impl Into<std::path::PathBuf>,
    ) -> Result<Self> {
        validate_configured_credential(credential)?;
        let reference = credential
            .secret_ref
            .strip_prefix("oauth-store:")
            .context("Claude subscription credential has an incompatible secret reference")?;
        if reference != credential.name {
            bail!("Claude subscription credential reference changed identity");
        }
        let store = Arc::new(FileOAuthCredentialStore::new(root.into()));
        let dialect = Arc::new(ClaudeOAuthDialect::production()?);
        let oauth = Arc::new(OAuthClient::new(dialect, store.clone())?);
        let expected_account = credential
            .account
            .clone()
            .context("Claude subscription credential omitted its bound account")?;
        Ok(Self {
            reference: reference.to_string(),
            expected_account: expected_account.clone(),
            store,
            oauth,
            refresh_lock: shared_refresh_lock(reference, &expected_account),
        })
    }

    fn load_bound(&self) -> Result<OAuthTokenRecord> {
        let record = self
            .store
            .load(&self.reference)?
            .context("Named Claude subscription credential is missing; sign in explicitly")?;
        diagnose_stored_claude_record(&record, &self.expected_account, &self.reference)?;
        self.oauth.validate_existing_binding(&record)?;
        Ok(record)
    }

    async fn refresh_generation(
        &self,
        rejected_generation: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<ClaudeCredentialLease> {
        let _guard = tokio::select! {
            _ = cancel.cancelled() => bail!("Claude subscription credential refresh was cancelled"),
            guard = self.refresh_lock.lock() => guard,
        };
        let current = self.load_bound()?;
        let needs_refresh = rejected_generation
            .map(|generation| generation == current.generation)
            .unwrap_or_else(|| current.expires_at <= Utc::now() + REFRESH_SKEW);
        if needs_refresh {
            self.oauth
                .refresh(&self.reference, cancel.clone())
                .await
                .context("Claude subscription credential refresh failed")?;
        }
        let refreshed = self.load_bound()?;
        self.oauth.validate_active_reuse(&refreshed)?;
        lease_from_record(refreshed)
    }
}

#[async_trait]
impl ClaudeCredentialSource for ProductionCredentialSource {
    async fn lease(&self, cancel: &CancellationToken) -> Result<ClaudeCredentialLease> {
        let record = self.load_bound()?;
        if record.expires_at <= Utc::now() + REFRESH_SKEW {
            return self.refresh_generation(None, cancel).await;
        }
        self.oauth.validate_active_reuse(&record)?;
        lease_from_record(record)
    }

    async fn refresh_after_unauthorized(
        &self,
        rejected_generation: &str,
        cancel: &CancellationToken,
    ) -> Result<ClaudeCredentialLease> {
        self.refresh_generation(Some(rejected_generation), cancel)
            .await
    }
}

/// Classify a stored Claude record before crypto or refresh.
///
/// `mutation_pending` is a crash marker from an interrupted refresh, not an
/// account change. Finch does not refresh in the background; a query that
/// starts rotation and then dies leaves this tombstone until explicit recover.
fn diagnose_stored_claude_record(
    record: &OAuthTokenRecord,
    expected_account: &str,
    reference: &str,
) -> Result<()> {
    if record.mutation_pending {
        bail!(
            "Claude credential `{reference}` has an interrupted token refresh \
             (crash marker `mutation_pending`). Finch does not refresh while idle; \
             a refresh that starts on a query and is interrupted will not retry \
             until you recover. Run `finch auth recover claude --credential {reference}` \
             then `finch auth login claude --credential {reference}`."
        );
    }
    if record.revoked {
        bail!(
            "Claude credential `{reference}` was revoked. \
             Run `finch auth login claude --credential {reference}` to sign in again."
        );
    }
    if record.access_token.is_empty() || record.generation.is_empty() {
        bail!(
            "Claude credential `{reference}` is missing usable token material. \
             Sign in again with `finch auth login claude --credential {reference}`."
        );
    }
    if record.account != expected_account {
        bail!(
            "Claude credential `{reference}` is bound to a different account than config. \
             Sign in again with `finch auth login claude --credential {reference}`."
        );
    }
    Ok(())
}

fn lease_from_record(record: OAuthTokenRecord) -> Result<ClaudeCredentialLease> {
    if record.access_token.is_empty() || record.account.is_empty() || record.generation.is_empty() {
        bail!("Claude subscription credential lease was invalid");
    }
    Ok(ClaudeCredentialLease {
        access_token: record.access_token.clone(),
        account: record.account.clone(),
        generation: record.generation.clone(),
    })
}

fn validate_configured_credential(credential: &ProviderCredential) -> Result<()> {
    if credential.provider != CredentialProvider::ClaudeSubscription
        || credential.audience != AudienceBinding::standard(EndpointFamily::ClaudeSubscription)
        || credential.issuer != "anthropic-claude"
        || credential.account.as_deref().is_none_or(str::is_empty)
    {
        bail!("Claude subscription provider and named credential binding do not match");
    }
    Ok(())
}

/// Finch-native Claude subscription transport.
///
/// Wraps the same `ClaudeProvider` Messages-API transport the API-key
/// `claude` adapter uses, authenticated with a leased/refreshed OAuth bearer
/// token instead of a static key.
pub struct ClaudeSubscriptionProvider {
    source: Arc<dyn ClaudeCredentialSource>,
    model: String,
    base_url: String,
}

impl fmt::Debug for ClaudeSubscriptionProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeSubscriptionProvider")
            .field("model", &self.model)
            .field("credential", &"[REDACTED]")
            .finish()
    }
}

impl ClaudeSubscriptionProvider {
    /// Construct the production Claude subscription lane from a named
    /// credential. `reasoning_effort` is accepted only for call-site parity
    /// with the other `Credentialed` subscription constructors; Claude has
    /// no configurable reasoning effort on this transport (mirrors the
    /// API-key `claude` adapter, which reports `ReasoningCapability::unsupported`),
    /// so a caller-supplied value is rejected rather than silently dropped.
    pub fn production(
        credential: &ProviderCredential,
        model: Option<&str>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<Self> {
        if reasoning_effort.is_some() {
            bail!(
                "Claude subscription does not support configurable reasoning effort; remove reasoning_effort from this profile"
            );
        }
        validate_configured_credential(credential)?;
        Ok(Self {
            source: Arc::new(ProductionCredentialSource::new(credential)?),
            model: model.unwrap_or(crate::DEFAULT_CLAUDE_MODEL).to_string(),
            base_url: CLAUDE_SUBSCRIPTION_BASE_URL.to_string(),
        })
    }

    #[cfg(test)]
    fn for_test(source: Arc<dyn ClaudeCredentialSource>, model: &str, base_url: &str) -> Self {
        Self {
            source,
            model: model.to_string(),
            base_url: base_url.to_string(),
        }
    }

    fn transport(&self, lease: &ClaudeCredentialLease) -> Result<ClaudeProvider> {
        let provider = ClaudeProvider::new_with_oauth_bearer(
            lease.access_token.clone(),
            &self.base_url,
            "/v1/messages",
            "/v1/models",
        )?
        .with_model(self.model.clone());
        Ok(provider)
    }

    fn classify_transport_error(error: anyhow::Error) -> anyhow::Error {
        let text = error.to_string().to_ascii_lowercase();
        if text.contains("401") || text.contains("unauthorized") {
            return SubscriptionUnauthorized.into();
        }
        error
    }

    async fn send_with_refresh<T, F, Fut>(&self, send: F) -> Result<T>
    where
        F: Fn(ClaudeProvider) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let cancel = CancellationToken::new();
        let lease = self.source.lease(&cancel).await?;
        match send(self.transport(&lease)?).await {
            Ok(value) => Ok(value),
            Err(error) => {
                let classified = Self::classify_transport_error(error);
                if classified
                    .downcast_ref::<SubscriptionUnauthorized>()
                    .is_none()
                {
                    return Err(classified);
                }
                let refreshed = self
                    .source
                    .refresh_after_unauthorized(&lease.generation, &cancel)
                    .await?;
                send(self.transport(&refreshed)?)
                    .await
                    .map_err(Self::classify_transport_error)
            }
        }
    }
}

#[async_trait]
impl ProviderBackend for ClaudeSubscriptionProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let (request, _bindings) = request.into_request_for(self)?;
        self.send_with_refresh(|provider| {
            let request = request.clone();
            async move { provider.send_message(&request).await }
        })
        .await
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<StreamChunk>>> {
        let (request, _bindings) = request.into_request_for(self)?;
        self.send_with_refresh(|provider| {
            let request = request.clone();
            async move { provider.send_message_stream(&request).await }
        })
        .await
    }

    fn name(&self) -> &str {
        "claude-sub"
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        if model != crate::DEFAULT_CLAUDE_MODEL {
            return ModelCapabilities::unknown(self.name(), model);
        }
        ModelCapabilities::static_metadata(
            self.name(),
            model,
            "2026-09-25",
            "Finch Claude subscription adapter; reuses the audited Anthropic Messages wire \
             protocol with an OAuth bearer token in place of an API key",
            CapabilitySupport::Supported,
            CapabilitySupport::Supported,
            CapabilitySupport::Unsupported,
            ReasoningCapability::unsupported("2026-09-25", "Finch Claude subscription adapter"),
            Some(1_000_000),
            Some(128_000),
            None,
        )
        .with_wire_protocol(
            WireProtocol::AnthropicMessages,
            "2026-09-25",
            "Finch Claude subscription adapter",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CredentialKind, CredentialLifecycle};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn credential() -> ProviderCredential {
        ProviderCredential {
            name: "claude:work".into(),
            kind: CredentialKind::OauthBrowserPkce,
            provider: CredentialProvider::ClaudeSubscription,
            issuer: "anthropic-claude".into(),
            audience: AudienceBinding::standard(EndpointFamily::ClaudeSubscription),
            tenant: None,
            project: None,
            account: Some("acct-work".into()),
            scopes: crate::claude_oauth::claude_required_scopes(),
            secret_ref: "oauth-store:claude:work".into(),
            lifecycle: CredentialLifecycle::Active {
                expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
                refreshable: true,
            },
            revocation: Default::default(),
        }
    }

    #[test]
    fn configured_credential_rejects_anthropic_api_key_records() {
        let mut api_key = credential();
        api_key.provider = CredentialProvider::Anthropic;
        api_key.issuer = "anthropic".into();
        api_key.audience = AudienceBinding::standard(EndpointFamily::AnthropicApi);
        api_key.kind = CredentialKind::ApiKey;
        let error = validate_configured_credential(&api_key)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("do not match"),
            "API-key records must not construct the subscription transport: {error}"
        );
    }

    #[test]
    fn explicit_reasoning_effort_is_rejected_not_silently_dropped() {
        let error = ClaudeSubscriptionProvider::production(
            &credential(),
            None,
            Some(ReasoningEffort::High),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("does not support configurable reasoning effort"));
    }

    #[test]
    fn unauthorized_errors_are_classified_distinctly_from_other_failures() {
        let unauthorized = ClaudeSubscriptionProvider::classify_transport_error(anyhow::anyhow!(
            "Claude API error 401 — Your Claude subscription sign-in may have expired: bad token"
        ));
        assert!(unauthorized
            .downcast_ref::<SubscriptionUnauthorized>()
            .is_some());
        let other = ClaudeSubscriptionProvider::classify_transport_error(anyhow::anyhow!(
            "Claude API error 429 — rate limited"
        ));
        assert!(other.downcast_ref::<SubscriptionUnauthorized>().is_none());
    }

    struct ScriptedSource {
        leases: AtomicUsize,
        refreshes: AtomicUsize,
        account: String,
    }

    #[async_trait]
    impl ClaudeCredentialSource for ScriptedSource {
        async fn lease(&self, _cancel: &CancellationToken) -> Result<ClaudeCredentialLease> {
            self.leases.fetch_add(1, Ordering::SeqCst);
            Ok(ClaudeCredentialLease {
                access_token: "stale-token".into(),
                account: self.account.clone(),
                generation: "gen-1".into(),
            })
        }

        async fn refresh_after_unauthorized(
            &self,
            rejected_generation: &str,
            _cancel: &CancellationToken,
        ) -> Result<ClaudeCredentialLease> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            assert_eq!(rejected_generation, "gen-1");
            Ok(ClaudeCredentialLease {
                access_token: "fresh-token".into(),
                account: self.account.clone(),
                generation: "gen-2".into(),
            })
        }
    }

    #[tokio::test]
    async fn a_rejected_lease_is_refreshed_exactly_once_then_retried() {
        let mut server = mockito::Server::new_async().await;
        let unauthorized_call = server
            .mock("POST", "/v1/messages")
            .match_header("authorization", "Bearer stale-token")
            .with_status(401)
            .with_body(r#"{"error":{"message":"expired"}}"#)
            .create_async()
            .await;
        let retried_call = server
            .mock("POST", "/v1/messages")
            .match_header("authorization", "Bearer fresh-token")
            .with_status(200)
            .with_body(r#"{"id":"m","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"model":"claude-sonnet-5","stop_reason":"end_turn"}"#)
            .create_async()
            .await;

        let source = Arc::new(ScriptedSource {
            leases: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
            account: "acct-work".into(),
        });
        let provider = ClaudeSubscriptionProvider::for_test(
            source.clone(),
            crate::DEFAULT_CLAUDE_MODEL,
            &server.url(),
        );

        let response = provider
            .send_with_refresh(|transport| async move {
                transport
                    .send_message(
                        &ProviderRequest::new(vec![crate::Message::user("hi")])
                            .with_model(crate::DEFAULT_CLAUDE_MODEL),
                    )
                    .await
            })
            .await
            .expect("a 401 on the stale lease must transparently refresh and retry");
        assert_eq!(response.model, "claude-sonnet-5");
        assert_eq!(source.leases.load(Ordering::SeqCst), 1);
        assert_eq!(
            source.refreshes.load(Ordering::SeqCst),
            1,
            "must refresh exactly once, not repeatedly, after the first 401"
        );
        unauthorized_call.assert_async().await;
        retried_call.assert_async().await;
    }
}
