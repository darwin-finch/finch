//! Finch-native SuperGrok subscription transport.
//!
//! Credentials, origin, catalog, and errors are not interchangeable with the
//! xAI Console API-key adapter. Session tokens are sent as `xai-grok-cli` to
//! `cli-chat-proxy.grok.com` and must never be presented as `Authorization:
//! Bearer` against `api.x.ai`.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use std::fmt;
use std::sync::{Arc, OnceLock, Weak};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::grok_oauth::{
    XaiGrokOAuthDialect, GROK_SESSION_TOKEN_HEADER, GROK_SUBSCRIPTION_BASE_URL,
};
use super::openai::OpenAIProvider;
use super::{
    CapabilitySupport, LlmProvider, ModelCapabilities, ProviderBackend, ProviderRequest,
    ProviderResponse, ReasoningCapability, StreamChunk, ValidatedProviderRequest, WireProtocol,
};
use crate::oauth::{FileOAuthCredentialStore, OAuthClient, OAuthCredentialStore, OAuthTokenRecord};
use crate::{
    AudienceBinding, CredentialProvider, EndpointFamily, ProviderCredential, ReasoningEffort,
};

const DEFAULT_MODEL: &str = "grok-4.6";
const DEFAULT_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::Medium;
const REFRESH_SKEW: ChronoDuration = ChronoDuration::minutes(2);

#[derive(Debug)]
struct SubscriptionUnauthorized;

impl fmt::Display for SubscriptionUnauthorized {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Grok subscription authorization was rejected")
    }
}

impl std::error::Error for SubscriptionUnauthorized {}

#[derive(Debug)]
struct SubscriptionWeeklyLimit;

impl fmt::Display for SubscriptionWeeklyLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "Grok subscription weekly usage limit was reached. This is not an xAI API credit failure. Configure an xAI API key as a separate provider if you want Console billing",
        )
    }
}

impl std::error::Error for SubscriptionWeeklyLimit {}

#[derive(Debug)]
struct SubscriptionApiCreditRejected;

impl fmt::Display for SubscriptionApiCreditRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "Grok subscription request was billed as xAI API credits. Finch will not silently switch to an API key. Use the grok API-key provider for Console billing",
        )
    }
}

impl std::error::Error for SubscriptionApiCreditRejected {}

pub struct GrokCredentialLease {
    access_token: String,
    account: String,
    generation: String,
}

impl fmt::Debug for GrokCredentialLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GrokCredentialLease([REDACTED])")
    }
}

#[async_trait]
pub trait GrokCredentialSource: Send + Sync {
    async fn lease(&self, cancel: &CancellationToken) -> Result<GrokCredentialLease>;
    async fn refresh_after_unauthorized(
        &self,
        rejected_generation: &str,
        cancel: &CancellationToken,
    ) -> Result<GrokCredentialLease>;
}

type ProductionOAuthClient =
    OAuthClient<XaiGrokOAuthDialect<crate::grok_jwks::GrokJwksVerifier>, FileOAuthCredentialStore>;

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
            .context("Grok subscription credential has an incompatible secret reference")?;
        if reference != credential.name {
            bail!("Grok subscription credential reference changed identity");
        }
        let store = Arc::new(FileOAuthCredentialStore::new(root.into()));
        let dialect = Arc::new(XaiGrokOAuthDialect::production()?);
        let oauth = Arc::new(OAuthClient::new(dialect, store.clone())?);
        let expected_account = credential
            .account
            .clone()
            .context("Grok subscription credential omitted its signed account")?;
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
            .context("Named Grok subscription credential is missing; sign in explicitly")?;
        diagnose_stored_grok_record(&record, &self.expected_account, &self.reference)?;
        self.oauth.validate_existing_binding(&record)?;
        Ok(record)
    }

    async fn refresh_generation(
        &self,
        rejected_generation: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<GrokCredentialLease> {
        let _guard = tokio::select! {
            _ = cancel.cancelled() => bail!("Grok subscription credential refresh was cancelled"),
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
                .context("Grok subscription credential refresh failed")?;
        }
        let refreshed = self.load_bound()?;
        self.oauth.validate_active_reuse(&refreshed)?;
        lease_from_record(refreshed)
    }
}

#[async_trait]
impl GrokCredentialSource for ProductionCredentialSource {
    async fn lease(&self, cancel: &CancellationToken) -> Result<GrokCredentialLease> {
        self.refresh_generation(None, cancel).await
    }

    async fn refresh_after_unauthorized(
        &self,
        rejected_generation: &str,
        cancel: &CancellationToken,
    ) -> Result<GrokCredentialLease> {
        self.refresh_generation(Some(rejected_generation), cancel)
            .await
    }
}

fn diagnose_stored_grok_record(
    record: &OAuthTokenRecord,
    expected_account: &str,
    reference: &str,
) -> Result<()> {
    if record.provider != CredentialProvider::GrokSubscription
        || record.audience != AudienceBinding::standard(EndpointFamily::GrokSubscription)
    {
        bail!("Named credential `{reference}` is not a Grok subscription record");
    }
    if record.account != expected_account {
        bail!(
            "Grok credential `{reference}` is bound to a different account than config. Sign in again"
        );
    }
    Ok(())
}

fn lease_from_record(record: OAuthTokenRecord) -> Result<GrokCredentialLease> {
    if record.access_token.is_empty() || record.account.is_empty() || record.generation.is_empty() {
        bail!("Grok subscription credential lease was invalid");
    }
    Ok(GrokCredentialLease {
        access_token: record.access_token.clone(),
        account: record.account.clone(),
        generation: record.generation.clone(),
    })
}

fn validate_configured_credential(credential: &ProviderCredential) -> Result<()> {
    if credential.provider != CredentialProvider::GrokSubscription
        || credential.audience != AudienceBinding::standard(EndpointFamily::GrokSubscription)
        || credential.issuer != "xai-grok"
        || credential.account.as_deref().is_none_or(str::is_empty)
    {
        bail!("Grok subscription provider and named credential binding do not match");
    }
    Ok(())
}

/// Finch-native SuperGrok subscription transport.
///
/// Session tokens go to `cli-chat-proxy.grok.com` as `xai-grok-cli`. They are
/// never presented as Console API keys against `api.x.ai`.
pub struct GrokSubscriptionProvider {
    source: Arc<dyn GrokCredentialSource>,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
    base_url: String,
}

impl fmt::Debug for GrokSubscriptionProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokSubscriptionProvider")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("base_url", &self.base_url)
            .field("credential", &"[REDACTED]")
            .finish()
    }
}

impl GrokSubscriptionProvider {
    /// Construct the production SuperGrok lane from a named credential.
    pub fn production(
        credential: &ProviderCredential,
        model: Option<&str>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<Self> {
        validate_configured_credential(credential)?;
        crate::grok_oauth::GrokSubscriptionService::default()
            .validate_endpoint(GROK_SUBSCRIPTION_BASE_URL)?;
        Ok(Self {
            source: Arc::new(ProductionCredentialSource::new(credential)?),
            model: model.unwrap_or(DEFAULT_MODEL).to_string(),
            reasoning_effort,
            base_url: GROK_SUBSCRIPTION_BASE_URL.to_string(),
        })
    }

    fn transport(&self, lease: &GrokCredentialLease) -> Result<OpenAIProvider> {
        crate::grok_oauth::GrokSubscriptionService::default().validate_endpoint(&self.base_url)?;
        let mut provider = OpenAIProvider::new_compatible_named_header(
            lease.access_token.clone(),
            self.base_url.clone(),
            "/v1/chat/completions",
            "/v1/models",
            self.model.clone(),
            "grok-sub".to_string(),
            GROK_SESSION_TOKEN_HEADER,
        )?;
        if let Some(effort) = self.reasoning_effort {
            provider = provider.with_reasoning_effort(effort);
        }
        Ok(provider)
    }

    fn classify_transport_error(error: anyhow::Error) -> anyhow::Error {
        let text = error.to_string().to_ascii_lowercase();
        if text.contains("weekly") || text.contains("rate limit") && text.contains("subscription") {
            return SubscriptionWeeklyLimit.into();
        }
        if text.contains("insufficient") && text.contains("credit")
            || text.contains("api credit")
            || text.contains("prepaid")
        {
            return SubscriptionApiCreditRejected.into();
        }
        if text.contains("401") || text.contains("unauthorized") {
            return SubscriptionUnauthorized.into();
        }
        error
    }

    async fn send_with_refresh<T, F, Fut>(&self, send: F) -> Result<T>
    where
        F: Fn(OpenAIProvider) -> Fut,
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
impl ProviderBackend for GrokSubscriptionProvider {
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
        "grok-sub"
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        ModelCapabilities::static_metadata(
            self.name(),
            model,
            "2026-09-16",
            "experimental grok-build public-client compatibility; not xAI Console API attestation",
            CapabilitySupport::Supported,
            CapabilitySupport::Supported,
            CapabilitySupport::Unsupported,
            ReasoningCapability::allowed(
                [
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                ],
                "2026-09-16",
                "experimental grok-build public-client compatibility; not xAI Console API attestation",
            ),
            Some(256_000),
            None,
            None,
        )
        .with_wire_protocol(
            WireProtocol::OpenAiChatCompletions,
            "2026-09-16",
            "Finch SuperGrok OpenAI-compatible chat-completions adapter",
        )
    }

    fn requested_reasoning_effort(&self, _request: &ProviderRequest) -> Option<ReasoningEffort> {
        self.reasoning_effort.or(Some(DEFAULT_REASONING_EFFORT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential() -> ProviderCredential {
        ProviderCredential {
            name: "grok-sub:work".into(),
            kind: crate::CredentialKind::OauthDevice,
            provider: CredentialProvider::GrokSubscription,
            issuer: "xai-grok".into(),
            audience: AudienceBinding::standard(EndpointFamily::GrokSubscription),
            tenant: None,
            project: None,
            account: Some("acct-work".into()),
            scopes: crate::grok_oauth::grok_required_scopes(),
            secret_ref: "oauth-store:grok-sub:work".into(),
            lifecycle: crate::CredentialLifecycle::Active {
                expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
                refreshable: true,
            },
            revocation: Default::default(),
        }
    }

    #[test]
    fn configured_credential_rejects_xai_api_key_records() {
        let mut api_key = credential();
        api_key.provider = CredentialProvider::Xai;
        api_key.issuer = "xai".into();
        api_key.audience = AudienceBinding::standard(EndpointFamily::XaiApi);
        api_key.kind = crate::CredentialKind::ApiKey;
        let error = validate_configured_credential(&api_key)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("do not match"),
            "API-key records must not construct the subscription transport: {error}"
        );
    }

    #[test]
    fn weekly_limit_and_api_credit_errors_stay_distinct() {
        let weekly = GrokSubscriptionProvider::classify_transport_error(anyhow::anyhow!(
            "API error 429: weekly limit exceeded"
        ));
        assert!(weekly.downcast_ref::<SubscriptionWeeklyLimit>().is_some());
        let credits = GrokSubscriptionProvider::classify_transport_error(anyhow::anyhow!(
            "API error 402: insufficient API credits"
        ));
        assert!(credits
            .downcast_ref::<SubscriptionApiCreditRejected>()
            .is_some());
        let credits = credits.to_string();
        assert!(
            credits.contains("will not silently switch"),
            "API-credit rejection must refuse silent fallback: {credits}"
        );
        assert!(
            weekly.to_string().contains("weekly usage limit"),
            "weekly limit must stay distinct from API-credit billing"
        );
    }
}
