//! Finch-native Google Gemini subscription transport.
//!
//! Credentials, origin, and errors are not interchangeable with the
//! Google AI Studio API-key adapter: a subscription lease is presented
//! as an OAuth Bearer token against `generativelanguage.googleapis.com`
//! and is never converted to or silently substituted for a static API key.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use std::fmt;
use std::sync::{Arc, OnceLock, Weak};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::gemini::GeminiProvider;
use super::gemini_oauth::{GeminiTokenVerifierProduction, GoogleGeminiOAuthDialect};
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

pub const DEFAULT_GEMINI_SUB_MODEL: &str = "gemini-2.5-flash";
const GEMINI_SUBSCRIPTION_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const REFRESH_SKEW: ChronoDuration = ChronoDuration::minutes(2);

#[derive(Debug)]
struct SubscriptionUnauthorized;

impl fmt::Display for SubscriptionUnauthorized {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Gemini subscription authorization was rejected")
    }
}

impl std::error::Error for SubscriptionUnauthorized {}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct GeminiCredentialLease {
    access_token: String,
    account: String,
    generation: String,
}

impl fmt::Debug for GeminiCredentialLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GeminiCredentialLease([REDACTED])")
    }
}

#[async_trait]
pub(crate) trait GeminiCredentialSource: Send + Sync {
    async fn lease(&self, cancel: &CancellationToken) -> Result<GeminiCredentialLease>;
    async fn refresh_after_unauthorized(
        &self,
        rejected_generation: &str,
        cancel: &CancellationToken,
    ) -> Result<GeminiCredentialLease>;
}

type ProductionOAuthClient =
    OAuthClient<GoogleGeminiOAuthDialect<GeminiTokenVerifierProduction>, FileOAuthCredentialStore>;

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
            .context("Gemini subscription credential has an incompatible secret reference")?;
        if reference != credential.name {
            bail!("Gemini subscription credential reference changed identity");
        }
        let store = Arc::new(FileOAuthCredentialStore::new(root.into()));
        let dialect = Arc::new(GoogleGeminiOAuthDialect::production()?);
        let oauth = Arc::new(OAuthClient::new(dialect, store.clone())?);
        let expected_account = credential
            .account
            .clone()
            .context("Gemini subscription credential omitted its bound account")?;
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
            .context("Named Gemini subscription credential is missing; sign in explicitly")?;
        diagnose_stored_gemini_record(&record, &self.expected_account, &self.reference)?;
        self.oauth.validate_existing_binding(&record)?;
        Ok(record)
    }

    async fn refresh_generation(
        &self,
        rejected_generation: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<GeminiCredentialLease> {
        let _guard = tokio::select! {
            _ = cancel.cancelled() => bail!("Gemini subscription credential refresh was cancelled"),
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
                .context("Gemini subscription credential refresh failed")?;
        }
        let refreshed = self.load_bound()?;
        self.oauth.validate_active_reuse(&refreshed)?;
        lease_from_record(refreshed)
    }
}

#[async_trait]
impl GeminiCredentialSource for ProductionCredentialSource {
    async fn lease(&self, cancel: &CancellationToken) -> Result<GeminiCredentialLease> {
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
    ) -> Result<GeminiCredentialLease> {
        self.refresh_generation(Some(rejected_generation), cancel)
            .await
    }
}

fn diagnose_stored_gemini_record(
    record: &OAuthTokenRecord,
    expected_account: &str,
    reference: &str,
) -> Result<()> {
    if record.mutation_pending {
        bail!(
            "Gemini credential `{reference}` has an interrupted token refresh \
             (crash marker `mutation_pending`). Finch does not refresh while idle; \
             a refresh that starts on a query and is interrupted will not retry \
             until you recover. Run `finch auth recover gemini-sub --credential {reference}` \
             then `finch auth login gemini-sub --credential {reference}`."
        );
    }
    if record.revoked {
        bail!(
            "Gemini credential `{reference}` was revoked. \
             Run `finch auth login gemini-sub --credential {reference}` to sign in again."
        );
    }
    if record.access_token.is_empty() || record.generation.is_empty() {
        bail!(
            "Gemini credential `{reference}` is missing usable token material. \
             Sign in again with `finch auth login gemini-sub --credential {reference}`."
        );
    }
    if record.account != expected_account {
        bail!(
            "Gemini credential `{reference}` is bound to a different account than config. \
             Sign in again with `finch auth login gemini-sub --credential {reference}`."
        );
    }
    Ok(())
}

fn lease_from_record(record: OAuthTokenRecord) -> Result<GeminiCredentialLease> {
    if record.access_token.is_empty() || record.account.is_empty() || record.generation.is_empty() {
        bail!("Gemini subscription credential lease was invalid");
    }
    Ok(GeminiCredentialLease {
        access_token: record.access_token.clone(),
        account: record.account.clone(),
        generation: record.generation.clone(),
    })
}

fn validate_configured_credential(credential: &ProviderCredential) -> Result<()> {
    if credential.provider != CredentialProvider::GeminiSubscription
        || credential.audience != AudienceBinding::standard(EndpointFamily::GeminiSubscription)
        || credential.issuer != "google-gemini"
        || credential.account.as_deref().is_none_or(str::is_empty)
    {
        bail!("Gemini subscription provider and named credential binding do not match");
    }
    Ok(())
}

/// Finch-native Gemini subscription transport.
///
/// Wraps `GeminiProvider` authenticated with a leased/refreshed OAuth bearer
/// token instead of a static API key.
pub struct GeminiSubscriptionProvider {
    source: Arc<dyn GeminiCredentialSource>,
    model: String,
    base_url: String,
}

impl fmt::Debug for GeminiSubscriptionProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeminiSubscriptionProvider")
            .field("model", &self.model)
            .field("credential", &"[REDACTED]")
            .finish()
    }
}

impl GeminiSubscriptionProvider {
    pub fn production(
        credential: &ProviderCredential,
        model: Option<&str>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<Self> {
        if reasoning_effort.is_some() {
            bail!(
                "Gemini subscription does not support configurable reasoning effort; remove reasoning_effort from this profile"
            );
        }
        validate_configured_credential(credential)?;
        Ok(Self {
            source: Arc::new(ProductionCredentialSource::new(credential)?),
            model: model.unwrap_or(DEFAULT_GEMINI_SUB_MODEL).to_string(),
            base_url: GEMINI_SUBSCRIPTION_BASE_URL.to_string(),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        source: Arc<dyn GeminiCredentialSource>,
        model: &str,
        base_url: &str,
    ) -> Self {
        Self {
            source,
            model: model.to_string(),
            base_url: base_url.to_string(),
        }
    }

    fn transport(&self, lease: &GeminiCredentialLease) -> Result<GeminiProvider> {
        let provider = GeminiProvider::new_with_auth(
            crate::gemini::GeminiAuth::OAuthBearer(lease.access_token.clone()),
            &self.base_url,
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
        F: Fn(GeminiProvider) -> Fut,
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
impl ProviderBackend for GeminiSubscriptionProvider {
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
        "gemini-sub"
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        if model != DEFAULT_GEMINI_SUB_MODEL {
            return ModelCapabilities::unknown(self.name(), model);
        }
        ModelCapabilities::static_metadata(
            self.name(),
            model,
            "2026-10-01",
            "Finch Gemini subscription adapter; reuses Google Generative Language wire \
             protocol with an OAuth bearer token in place of an API key",
            CapabilitySupport::Unknown,
            CapabilitySupport::Supported,
            CapabilitySupport::Unsupported,
            ReasoningCapability::unsupported("2026-10-01", "Finch Gemini subscription adapter"),
            Some(1_048_576),
            Some(65_536),
            None,
        )
        .with_wire_protocol(
            WireProtocol::GeminiGenerateContent,
            "2026-10-01",
            "Finch Gemini subscription adapter",
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
            name: "gemini:work".into(),
            kind: CredentialKind::OauthDevice,
            provider: CredentialProvider::GeminiSubscription,
            issuer: "google-gemini".into(),
            audience: AudienceBinding::standard(EndpointFamily::GeminiSubscription),
            tenant: None,
            project: None,
            account: Some("user@example.com".into()),
            scopes: crate::gemini_oauth::gemini_required_scopes(),
            secret_ref: "oauth-store:gemini:work".into(),
            lifecycle: CredentialLifecycle::Active {
                expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
                refreshable: true,
            },
            revocation: Default::default(),
        }
    }

    #[test]
    fn configured_credential_rejects_mismatched_provider() {
        let mut api_key = credential();
        api_key.provider = CredentialProvider::GeminiAiStudio;
        assert!(validate_configured_credential(&api_key).is_err());
    }

    #[test]
    fn configured_credential_validates_correctly() {
        assert!(validate_configured_credential(&credential()).is_ok());
    }

    struct MockCredentialSource {
        lease_count: AtomicUsize,
        refresh_count: AtomicUsize,
        should_fail_first: bool,
    }

    #[async_trait]
    impl GeminiCredentialSource for MockCredentialSource {
        async fn lease(&self, _cancel: &CancellationToken) -> Result<GeminiCredentialLease> {
            self.lease_count.fetch_add(1, Ordering::SeqCst);
            Ok(GeminiCredentialLease {
                access_token: if self.should_fail_first
                    && self.refresh_count.load(Ordering::SeqCst) == 0
                {
                    "stale_token".into()
                } else {
                    "valid_token".into()
                },
                account: "user@example.com".into(),
                generation: "gen-1".into(),
            })
        }

        async fn refresh_after_unauthorized(
            &self,
            _rejected_generation: &str,
            _cancel: &CancellationToken,
        ) -> Result<GeminiCredentialLease> {
            self.refresh_count.fetch_add(1, Ordering::SeqCst);
            Ok(GeminiCredentialLease {
                access_token: "valid_token".into(),
                account: "user@example.com".into(),
                generation: "gen-2".into(),
            })
        }
    }

    #[tokio::test]
    async fn send_with_refresh_retries_on_unauthorized() {
        let source = Arc::new(MockCredentialSource {
            lease_count: AtomicUsize::new(0),
            refresh_count: AtomicUsize::new(0),
            should_fail_first: true,
        });
        let provider = GeminiSubscriptionProvider::for_test(
            source.clone(),
            "gemini-2.5-flash",
            "http://127.0.0.1:9",
        );

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result: Result<String> = provider
            .send_with_refresh(|_provider| {
                let call_count = call_count_clone.clone();
                async move {
                    let count = call_count.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        bail!("HTTP 401 Unauthorized");
                    }
                    Ok("success".to_string())
                }
            })
            .await;

        assert_eq!(result.unwrap(), "success");
        assert_eq!(source.lease_count.load(Ordering::SeqCst), 1);
        assert_eq!(source.refresh_count.load(Ordering::SeqCst), 1);
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }
}
