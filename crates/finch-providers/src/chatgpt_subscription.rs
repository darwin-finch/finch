//! Finch-native ChatGPT subscription Responses-Lite transport.
//!
//! This is a compatibility adapter pinned to an exact public OpenAI Codex
//! source revision. It is deliberately separate from the OpenAI Platform
//! adapter: credentials, origin, catalog, request dialect, allowance, and
//! errors are not interchangeable.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use futures::StreamExt;
use reqwest::{Client, Response, StatusCode, Url};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::chatgpt_oauth::{OpenAiChatGptOAuthDialect, CHATGPT_SUBSCRIPTION_BASE_URL};
use super::{
    CapabilitySupport, ModelCapabilities, ModelFeature, ProviderBackend, ProviderRequest,
    ProviderResponse, ReasoningCapability, StreamChunk, ValidatedProviderRequest, WireProtocol,
};
use crate::oauth::{FileOAuthCredentialStore, OAuthClient, OAuthCredentialStore, OAuthTokenRecord};
#[cfg(test)]
use crate::tool_bindings::compile_from_definitions;
use crate::tool_bindings::{ToolBindingError, ToolBindingTable};
#[cfg(test)]
use crate::ToolDefinition;
use crate::{
    AudienceBinding, CredentialProvider, EndpointFamily, ProviderCredential, ReasoningEffort,
};
use crate::{ContentBlock, Message};

pub const CHATGPT_INFERENCE_PROTOCOL_REVISION: &str =
    "openai-codex-responses-lite@6478a751fde8884b2fdc76486fe23175a8e795d4";
// The catalog service filters models by the Codex protocol version, not by
// Finch's product version. This is the released Codex version corresponding to
// the public source revision pinned above. Update both pins together after a
// protocol audit and live acceptance run.
const CHATGPT_CATALOG_CLIENT_VERSION: &str = "0.151.0";
const FINCH_CHATGPT_USER_AGENT: &str = concat!(
    "finch/",
    env!("CARGO_PKG_VERSION"),
    " (+https://darwin-finch.github.io/)"
);
const DEFAULT_MODEL: &str = "gpt-5.6-sol";
const MODEL_ALIAS: &str = "gpt-5.6";
const DEFAULT_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::Medium;
const RESPONSES_PATH: &str = "/backend-api/codex/responses";
const MODELS_PATH: &str = "/backend-api/codex/models";
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;
const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_CATALOG_CONTEXT_WINDOW: u64 = 10_000_000;
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;
const MAX_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_USAGE_METADATA_BYTES: usize = 256 * 1024;
const MAX_OPAQUE_REASONING_BYTES: usize = 4 * 1024 * 1024;
const MAX_OUTPUT_ITEMS: usize = 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CATALOG_TTL: Duration = Duration::from_secs(5 * 60);
const REFRESH_SKEW: ChronoDuration = ChronoDuration::minutes(2);
// Preserve the provider contract of 32 caller-visible buffered chunks. The
// physical channel has one additional slot which is reserved before the
// receiver is returned, exclusively for an unambiguous terminal error.
const STREAM_BUFFER_CAPACITY: usize = 32;
const STREAM_CHANNEL_CAPACITY: usize = STREAM_BUFFER_CAPACITY + 1;

#[cfg(test)]
#[derive(Clone, Debug)]
enum StreamProducerEvent {
    SendAttempt(StreamChunk),
    /// The producer task's frame was destroyed. `panicking` distinguishes a
    /// clean return from an unwind: tokio swallows a panic inside a spawned
    /// task, and the transport is released either way, so without this flag a
    /// panicking producer is indistinguishable from a terminating one for any
    /// test that does not drain the channel.
    Finished {
        panicking: bool,
    },
}

#[cfg(test)]
type StreamProducerObserver = Arc<dyn Fn(StreamProducerEvent) + Send + Sync>;

#[cfg(test)]
struct StreamProducerFinishGuard(Option<StreamProducerObserver>);

#[cfg(test)]
impl Drop for StreamProducerFinishGuard {
    fn drop(&mut self) {
        if let Some(observer) = self.0.as_ref() {
            observer(StreamProducerEvent::Finished {
                panicking: std::thread::panicking(),
            });
        }
    }
}

#[derive(Debug)]
struct SubscriptionUnauthorized;

impl fmt::Display for SubscriptionUnauthorized {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChatGPT subscription authorization was rejected")
    }
}

impl std::error::Error for SubscriptionUnauthorized {}

#[derive(Debug)]
struct SubscriptionCatalogUnavailable;

impl fmt::Display for SubscriptionCatalogUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ChatGPT subscription returned no models for pinned Codex compatibility version {CHATGPT_CATALOG_CLIENT_VERSION}; account entitlement or server compatibility filtering may have excluded the catalog"
        )
    }
}

impl std::error::Error for SubscriptionCatalogUnavailable {}

#[derive(Debug)]
enum SubscriptionCatalogContextWindowInvalid {
    MissingOrMalformed,
    OutOfBounds(u64),
}

impl fmt::Display for SubscriptionCatalogContextWindowInvalid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOrMalformed => formatter.write_str(
                "ChatGPT catalog model context window was missing or malformed",
            ),
            Self::OutOfBounds(value) => write!(
                formatter,
                "ChatGPT catalog model context window {value} was outside the supported range 1..={MAX_CATALOG_CONTEXT_WINDOW}"
            ),
        }
    }
}

impl std::error::Error for SubscriptionCatalogContextWindowInvalid {}

#[derive(Debug)]
struct SubscriptionCatalogNoSelectableModel;

impl fmt::Display for SubscriptionCatalogNoSelectableModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "ChatGPT account catalog did not advertise a supported GPT-5.6 Sol identifier",
        )
    }
}

impl std::error::Error for SubscriptionCatalogNoSelectableModel {}

#[derive(Debug)]
struct SubscriptionRequestedModelUnavailable;

impl fmt::Display for SubscriptionRequestedModelUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChatGPT account does not advertise the configured supported model")
    }
}

impl std::error::Error for SubscriptionRequestedModelUnavailable {}

#[derive(Debug)]
struct SubscriptionResponseRejected(StatusCode);

impl fmt::Display for SubscriptionResponseRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ChatGPT subscription rejected the pinned Responses-Lite request (HTTP {}); the account entitlement or pinned protocol contract may have changed",
            self.0
        )
    }
}

impl std::error::Error for SubscriptionResponseRejected {}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ChatGptCredentialLease {
    access_token: String,
    account: String,
    generation: String,
}

impl fmt::Debug for ChatGptCredentialLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChatGptCredentialLease([REDACTED])")
    }
}

#[async_trait]
pub trait ChatGptCredentialSource: Send + Sync {
    async fn lease(&self, cancel: &CancellationToken) -> Result<ChatGptCredentialLease>;
    async fn refresh_after_unauthorized(
        &self,
        rejected_generation: &str,
        cancel: &CancellationToken,
    ) -> Result<ChatGptCredentialLease>;
}

type ProductionOAuthClient = OAuthClient<
    OpenAiChatGptOAuthDialect<crate::openai_jwks::OpenAiJwksVerifier>,
    FileOAuthCredentialStore,
>;

struct ProductionCredentialSource {
    reference: String,
    expected_account: String,
    store: Arc<FileOAuthCredentialStore>,
    oauth: Arc<ProductionOAuthClient>,
    refresh_lock: Arc<Mutex<()>>,
}

fn shared_refresh_lock(reference: &str, account: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
    let key = format!("{reference}\0{account}");
    let mut locks = LOCKS
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
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
            .context("ChatGPT subscription credential has an incompatible secret reference")?;
        if reference != credential.name {
            bail!("ChatGPT subscription credential reference changed identity");
        }
        let store = Arc::new(FileOAuthCredentialStore::new(root.into()));
        let dialect = Arc::new(OpenAiChatGptOAuthDialect::production()?);
        let oauth = Arc::new(OAuthClient::new(dialect, store.clone())?);
        let expected_account = credential
            .account
            .clone()
            .context("ChatGPT subscription credential omitted its signed account")?;
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
            .context("Named ChatGPT subscription credential is missing; sign in explicitly")?;
        self.oauth.validate_existing_binding(&record)?;
        if record.account != self.expected_account
            || record.revoked
            || record.mutation_pending
            || record.access_token.is_empty()
            || record.generation.is_empty()
        {
            bail!("Named ChatGPT subscription credential changed accounts");
        }
        Ok(record)
    }

    async fn refresh_generation(
        &self,
        rejected_generation: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<ChatGptCredentialLease> {
        let _guard = tokio::select! {
            _ = cancel.cancelled() => bail!("ChatGPT subscription credential refresh was cancelled"),
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
                .context("ChatGPT subscription credential refresh failed")?;
        }
        let refreshed = self.load_bound()?;
        self.oauth.validate_active_reuse(&refreshed)?;
        lease_from_record(refreshed)
    }
}

#[async_trait]
impl ChatGptCredentialSource for ProductionCredentialSource {
    async fn lease(&self, cancel: &CancellationToken) -> Result<ChatGptCredentialLease> {
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
    ) -> Result<ChatGptCredentialLease> {
        self.refresh_generation(Some(rejected_generation), cancel)
            .await
    }
}

fn lease_from_record(record: OAuthTokenRecord) -> Result<ChatGptCredentialLease> {
    if record.access_token.is_empty() || record.account.is_empty() || record.generation.is_empty() {
        bail!("ChatGPT subscription credential lease was invalid");
    }
    Ok(ChatGptCredentialLease {
        access_token: record.access_token.clone(),
        account: record.account.clone(),
        generation: record.generation.clone(),
    })
}

fn validate_configured_credential(credential: &ProviderCredential) -> Result<()> {
    if credential.provider != CredentialProvider::ChatgptSubscription
        || credential.audience != AudienceBinding::standard(EndpointFamily::ChatgptSubscription)
        || credential.issuer != "openai-chatgpt"
        || credential.account.as_deref().is_none_or(str::is_empty)
    {
        bail!("ChatGPT subscription provider and named credential binding do not match");
    }
    Ok(())
}

#[derive(Clone)]
struct CatalogCache {
    generation: String,
    account: String,
    etag: Option<String>,
    catalog: Catalog,
    fetched_at: tokio::time::Instant,
}

#[derive(Clone)]
struct Catalog {
    models: BTreeMap<String, CatalogModel>,
}

#[derive(Clone)]
struct CatalogModel {
    slug: String,
    context_window: usize,
    image_input: bool,
    responses_lite: bool,
}

pub struct ChatGptSubscriptionProvider {
    client: Client,
    source: Arc<dyn ChatGptCredentialSource>,
    model: String,
    reasoning_effort: ReasoningEffort,
    base: Url,
    allow_loopback: bool,
    catalog: Mutex<Option<CatalogCache>>,
    #[cfg(test)]
    stream_producer_observer: Option<StreamProducerObserver>,
}

impl fmt::Debug for ChatGptSubscriptionProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatGptSubscriptionProvider")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("protocol", &CHATGPT_INFERENCE_PROTOCOL_REVISION)
            .field("credential", &"[REDACTED]")
            .finish()
    }
}

impl ChatGptSubscriptionProvider {
    pub fn production(
        credential: &ProviderCredential,
        model: Option<&str>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<Self> {
        validate_configured_credential(credential)?;
        Self::new(
            Arc::new(ProductionCredentialSource::new(credential)?),
            CHATGPT_SUBSCRIPTION_BASE_URL,
            model.unwrap_or(DEFAULT_MODEL),
            reasoning_effort.unwrap_or(DEFAULT_REASONING_EFFORT),
            false,
        )
    }

    #[cfg(test)]
    fn production_in_oauth_root(
        credential: &ProviderCredential,
        model: Option<&str>,
        reasoning_effort: Option<ReasoningEffort>,
        oauth_root: impl Into<std::path::PathBuf>,
    ) -> Result<Self> {
        validate_configured_credential(credential)?;
        Self::new(
            Arc::new(ProductionCredentialSource::new_in_root(
                credential, oauth_root,
            )?),
            CHATGPT_SUBSCRIPTION_BASE_URL,
            model.unwrap_or(DEFAULT_MODEL),
            reasoning_effort.unwrap_or(DEFAULT_REASONING_EFFORT),
            false,
        )
    }

    fn new(
        source: Arc<dyn ChatGptCredentialSource>,
        base: &str,
        model: &str,
        reasoning_effort: ReasoningEffort,
        allow_loopback: bool,
    ) -> Result<Self> {
        validate_model(model)?;
        validate_reasoning(reasoning_effort)?;
        let base = validate_base(base, allow_loopback)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("Failed to construct ChatGPT subscription HTTP client")?;
        Ok(Self {
            client,
            source,
            model: model.to_string(),
            reasoning_effort,
            base,
            allow_loopback,
            catalog: Mutex::new(None),
            #[cfg(test)]
            stream_producer_observer: None,
        })
    }

    #[cfg(test)]
    fn for_test(source: Arc<dyn ChatGptCredentialSource>, base: &str, model: &str) -> Result<Self> {
        Self::new(source, base, model, ReasoningEffort::High, true)
    }

    #[cfg(test)]
    fn with_stream_producer_observer(mut self, observer: StreamProducerObserver) -> Self {
        self.stream_producer_observer = Some(observer);
        self
    }

    fn route(&self, path: &str, query: Option<(&str, &str)>) -> Result<Url> {
        let mut url = self.base.clone();
        url.set_path(path);
        url.set_query(None);
        if let Some((name, value)) = query {
            url.query_pairs_mut().append_pair(name, value);
        }
        validate_route(&url, path, query.map(|(name, _)| name), self.allow_loopback)?;
        Ok(url)
    }

    async fn account_catalog(
        &self,
        lease: &ChatGptCredentialLease,
        cancel: &CancellationToken,
    ) -> Result<Catalog> {
        let mut cache = tokio::select! {
            _ = cancel.cancelled() => bail!("ChatGPT subscription model discovery was cancelled"),
            cache = self.catalog.lock() => cache,
        };
        if let Some(entry) = cache.as_ref() {
            if entry.generation == lease.generation
                && entry.account == lease.account
                && entry.fetched_at.elapsed() <= CATALOG_TTL
            {
                return Ok(entry.catalog.clone());
            }
        }
        let etag = cache
            .as_ref()
            .filter(|entry| entry.account == lease.account)
            .and_then(|entry| entry.etag.clone());
        let url = self.route(
            MODELS_PATH,
            Some(("client_version", CHATGPT_CATALOG_CLIENT_VERSION)),
        )?;
        let mut request = self
            .client
            .get(url)
            .bearer_auth(&lease.access_token)
            .header("ChatGPT-Account-ID", &lease.account)
            .header("originator", "finch")
            .header(reqwest::header::USER_AGENT, FINCH_CHATGPT_USER_AGENT)
            .header("version", CHATGPT_CATALOG_CLIENT_VERSION)
            .header(
                "x-finch-chatgpt-protocol",
                CHATGPT_INFERENCE_PROTOCOL_REVISION,
            );
        if let Some(etag) = etag.as_deref() {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let response = tokio::select! {
            _ = cancel.cancelled() => bail!("ChatGPT subscription model discovery was cancelled"),
            response = request.send() => response.context("ChatGPT subscription model discovery failed")?,
        };
        if response.status() == StatusCode::NOT_MODIFIED {
            let entry = cache
                .as_mut()
                .context("ChatGPT subscription returned 304 without an account catalog")?;
            if entry.account != lease.account {
                bail!("ChatGPT subscription catalog account changed");
            }
            entry.generation = lease.generation.clone();
            entry.fetched_at = tokio::time::Instant::now();
            return Ok(entry.catalog.clone());
        }
        let status = response.status();
        let response_etag = bounded_header(response.headers(), reqwest::header::ETAG.as_str())?;
        let body = read_bounded(response, MAX_CATALOG_BYTES, cancel).await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(SubscriptionUnauthorized.into());
        }
        if !status.is_success() {
            bail!("ChatGPT subscription model discovery failed (HTTP {status})");
        }
        let catalog = parse_catalog(&body)?;
        *cache = Some(CatalogCache {
            generation: lease.generation.clone(),
            account: lease.account.clone(),
            etag: response_etag,
            catalog: catalog.clone(),
            fetched_at: tokio::time::Instant::now(),
        });
        Ok(catalog)
    }

    async fn start_response(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
        bindings: &ToolBindingTable,
    ) -> Result<Response> {
        let body = responses_lite_request(&request, self.reasoning_effort, bindings)?;
        let body =
            serde_json::to_vec(&body).context("Failed to encode ChatGPT subscription request")?;
        if body.len() > MAX_REQUEST_BYTES {
            bail!("ChatGPT subscription request exceeded the size limit");
        }
        let mut lease = self.source.lease(&cancel).await?;
        let mut unauthorized_retry_used = false;
        let mut catalog = match self.account_catalog(&lease, &cancel).await {
            Ok(catalog) => catalog,
            Err(error) if error.downcast_ref::<SubscriptionUnauthorized>().is_some() => {
                lease = self
                    .source
                    .refresh_after_unauthorized(&lease.generation, &cancel)
                    .await?;
                unauthorized_retry_used = true;
                self.account_catalog(&lease, &cancel).await?
            }
            Err(error) => return Err(error),
        };
        let selected = catalog
            .models
            .get(&request.model)
            .ok_or(SubscriptionRequestedModelUnavailable)?;
        if !catalog_model_matches_request(selected, &request.model) {
            bail!("ChatGPT account model is not compatible with the pinned Responses-Lite dialect");
        }
        let url = self.route(RESPONSES_PATH, None)?;
        for _ in 0..2 {
            let response = tokio::select! {
                _ = cancel.cancelled() => bail!("ChatGPT subscription request was cancelled"),
                response = self.client.post(url.clone())
                    .bearer_auth(&lease.access_token)
                    .header("ChatGPT-Account-ID", &lease.account)
                    .header("originator", "finch")
                    .header(reqwest::header::USER_AGENT, FINCH_CHATGPT_USER_AGENT)
                    .header("version", CHATGPT_CATALOG_CLIENT_VERSION)
                    .header("x-finch-chatgpt-protocol", CHATGPT_INFERENCE_PROTOCOL_REVISION)
                    .header("x-openai-internal-codex-responses-lite", "true")
                    .header(reqwest::header::ACCEPT, "text/event-stream")
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send() => response.context("Failed to start ChatGPT subscription response")?,
            };
            if response.status() == StatusCode::UNAUTHORIZED && !unauthorized_retry_used {
                let _ = read_bounded(response, MAX_ERROR_BYTES, &cancel).await?;
                lease = self
                    .source
                    .refresh_after_unauthorized(&lease.generation, &cancel)
                    .await?;
                catalog = self.account_catalog(&lease, &cancel).await?;
                let refreshed_model = catalog
                    .models
                    .get(&request.model)
                    .ok_or(SubscriptionRequestedModelUnavailable)?;
                if !catalog_model_matches_request(refreshed_model, &request.model) {
                    bail!("ChatGPT account model changed while refreshing credentials");
                }
                unauthorized_retry_used = true;
                continue;
            }
            if !response.status().is_success() {
                let status = response.status();
                let _ = read_bounded(response, MAX_ERROR_BYTES, &cancel).await?;
                return Err(SubscriptionResponseRejected(status).into());
            }
            let content_type =
                bounded_header(response.headers(), reqwest::header::CONTENT_TYPE.as_str())?;
            // The ChatGPT Codex backend may omit Content-Type on a successful
            // streaming response. In that case the bounded SSE parser remains
            // the authoritative validator.
            if content_type
                .as_deref()
                .is_some_and(|value| !value.starts_with("text/event-stream"))
            {
                let _ = read_bounded(response, MAX_ERROR_BYTES, &cancel).await?;
                bail!("ChatGPT subscription response was not an event stream");
            }
            return Ok(response);
        }
        unreachable!("two bounded attempts either return or fail")
    }
}

#[async_trait]
impl ProviderBackend for ChatGptSubscriptionProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let (request, bindings) = request.into_request_for(self)?;
        let expected_model = request.model.clone();
        let response = self
            .start_response(request, CancellationToken::new(), &bindings)
            .await?;
        let completed = consume_sse(
            response,
            None,
            CancellationToken::new(),
            expected_model,
            (*bindings).clone(),
            #[cfg(test)]
            self.stream_producer_observer.clone(),
        )
        .await?;
        Ok(ProviderResponse {
            id: completed.id,
            model: completed.model,
            content: completed.blocks,
            stop_reason: Some("end_turn".to_string()),
            role: "assistant".to_string(),
            provider: "chatgpt_subscription".to_string(),
            usage: match (completed.input_tokens, completed.output_tokens) {
                (Some(input_tokens), Some(output_tokens)) => Some(super::ProviderUsage {
                    input_tokens,
                    output_tokens,
                }),
                (None, None) => None,
                _ => bail!("ChatGPT subscription returned incomplete usage metadata"),
            },
            allowance: completed
                .allowance
                .map(|allowance| super::ProviderAllowance {
                    primary_used_percent: allowance.primary_used_percent,
                    secondary_used_percent: allowance.secondary_used_percent,
                }),
        })
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let (request, bindings) = request.into_request_for(self)?;
        let expected_model = request.model.clone();
        let cancel = request.cancellation_token.clone().unwrap_or_default();
        let response = self
            .start_response(request, cancel.clone(), &bindings)
            .await?;
        let (sender, receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        // Reserve the channel's dedicated terminal slot before exposing the
        // receiver. Callers retain the historical 32-chunk ordinary buffer,
        // while cancellation or protocol failure can always publish exactly
        // one final error under complete backpressure.
        let terminal_error = sender
            .clone()
            .try_reserve_owned()
            .map_err(|_| anyhow::anyhow!("Failed to reserve ChatGPT terminal stream capacity"))?;
        #[cfg(test)]
        let stream_producer_observer = self.stream_producer_observer.clone();
        let stream_bindings = (*bindings).clone();
        tokio::spawn(async move {
            #[cfg(test)]
            let _finish_guard = StreamProducerFinishGuard(stream_producer_observer.clone());
            if let Err(error) = consume_sse(
                response,
                Some(sender),
                cancel,
                expected_model,
                stream_bindings,
                #[cfg(test)]
                stream_producer_observer,
            )
            .await
            {
                // The permit makes terminal failure publication non-blocking.
                // Its returned sender is dropped immediately so the receiver
                // observes this one error followed by channel closure.
                drop(terminal_error.send(Err(anyhow::anyhow!(error.to_string()))));
            }
        });
        Ok(receiver)
    }

    fn name(&self) -> &str {
        "chatgpt_subscription"
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        subscription_capabilities(model)
    }

    fn requested_reasoning_effort(&self, _request: &ProviderRequest) -> Option<ReasoningEffort> {
        Some(self.reasoning_effort)
    }
}

fn subscription_capabilities(model: &str) -> ModelCapabilities {
    if !matches!(model, DEFAULT_MODEL | MODEL_ALIAS) {
        return ModelCapabilities::unknown("chatgpt_subscription", model);
    }
    let source = CHATGPT_INFERENCE_PROTOCOL_REVISION;
    let mut capabilities = ModelCapabilities::static_metadata(
        "chatgpt_subscription",
        model,
        "2026-08-30",
        source,
        CapabilitySupport::Supported,
        CapabilitySupport::Supported,
        CapabilitySupport::Supported,
        ReasoningCapability::allowed(
            [
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::Xhigh,
                ReasoningEffort::Max,
            ],
            "2026-08-30",
            source,
        ),
        // The account-scoped catalog is authoritative for this value. The
        // synchronous capability API cannot perform credentialed discovery.
        None,
        Some(128_000),
        None,
    )
    .with_wire_protocol(
        WireProtocol::OpenAiChatGptResponsesLite,
        "2026-08-30",
        source,
    );
    capabilities.parallel_tool_calls =
        ModelFeature::static_metadata(CapabilitySupport::Unsupported, "2026-08-30", source);
    capabilities.image_input =
        ModelFeature::static_metadata(CapabilitySupport::Supported, "2026-08-30", source);
    capabilities.usage_reporting =
        ModelFeature::static_metadata(CapabilitySupport::Supported, "2026-08-30", source);
    capabilities
}

fn catalog_model_matches_request(model: &CatalogModel, requested_model: &str) -> bool {
    model.responses_lite
        && model.image_input
        && (1..=MAX_CATALOG_CONTEXT_WINDOW as usize).contains(&model.context_window)
        && model.slug == requested_model
}

fn validate_model(model: &str) -> Result<()> {
    if !matches!(model, DEFAULT_MODEL | MODEL_ALIAS) {
        bail!("ChatGPT subscription supports only the pinned GPT-5.6 Sol catalog entries");
    }
    Ok(())
}

fn validate_reasoning(effort: ReasoningEffort) -> Result<()> {
    if !matches!(
        effort,
        ReasoningEffort::Low
            | ReasoningEffort::Medium
            | ReasoningEffort::High
            | ReasoningEffort::Xhigh
            | ReasoningEffort::Max
    ) {
        bail!(
            "ChatGPT GPT-5.6 Sol does not support reasoning effort '{}' on the pinned Responses-Lite route; allowed efforts: low, medium, high, xhigh, max",
            effort.as_str()
        );
    }
    Ok(())
}

fn validate_base(value: &str, allow_loopback: bool) -> Result<Url> {
    let url = Url::parse(value).context("Invalid ChatGPT subscription service URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path().trim_end_matches('/') != "/backend-api/codex"
    {
        bail!("ChatGPT subscription service URL changed from the pinned origin");
    }
    let production =
        url.scheme() == "https" && url.host_str() == Some("chatgpt.com") && url.port().is_none();
    let loopback = allow_loopback
        && url.scheme() == "http"
        && url
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|address| address.is_loopback());
    if !production && !loopback {
        bail!("ChatGPT subscription credentials may only use the pinned chatgpt.com service");
    }
    Ok(url)
}

fn validate_route(url: &Url, path: &str, query: Option<&str>, allow_loopback: bool) -> Result<()> {
    let base = format!(
        "{}://{}{}{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default(),
        "/backend-api/codex"
    );
    validate_base(&base, allow_loopback)?;
    if url.path() != path
        || url.fragment().is_some()
        || url.username() != ""
        || url.password().is_some()
        || match query {
            None => url.query().is_some(),
            Some(name) => {
                url.query_pairs().count() != 1
                    || url.query_pairs().next().map(|v| v.0) != Some(name.into())
            }
        }
    {
        bail!("ChatGPT subscription request route changed from the pinned protocol");
    }
    Ok(())
}

#[cfg(test)]
fn encode_responses_lite(request: &ProviderRequest, effort: ReasoningEffort) -> Result<Value> {
    let bindings = chatgpt_bindings(request)?;
    responses_lite_request(request, effort, &bindings)
}

fn responses_lite_request(
    request: &ProviderRequest,
    effort: ReasoningEffort,
    bindings: &ToolBindingTable,
) -> Result<Value> {
    validate_model(&request.model)?;
    validate_reasoning(effort)?;
    if request.temperature.is_some() {
        bail!("ChatGPT Responses-Lite does not accept Finch temperature overrides");
    }
    let tools = map_tools(bindings)?;
    let mut input = Vec::new();
    let tools_payload = serde_json::to_vec(&tools)
        .context("Failed to encode ChatGPT subscription tool definitions")?;
    input.push(json!({
        "id": responses_lite_prefix_id("at", &tools_payload),
        "type":"additional_tools",
        "role":"developer",
        "tools":tools
    }));
    if let Some(system) = request.system.as_deref().filter(|value| !value.is_empty()) {
        validate_bounded_text(system, MAX_TOOL_ARGUMENT_BYTES, "instructions")?;
        input.push(json!({
            "id": responses_lite_prefix_id("msg", system.as_bytes()),
            "type":"message","role":"developer",
            "content":[{"type":"input_text","text":system}]
        }));
    }
    let mut calls = HashSet::new();
    let mut results = HashSet::new();
    for message in &request.messages {
        map_message(message, &mut input, &mut calls, &mut results, bindings)?;
    }
    if input.len() == 1 && request.system.as_deref().is_none_or(str::is_empty) {
        bail!("ChatGPT subscription request omitted conversation input");
    }
    if !results.is_subset(&calls) {
        bail!("ChatGPT subscription request contained an unmatched tool result");
    }
    let body = json!({
        "model": request.model,
        "input": input,
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "reasoning": {"effort":effort.as_str(),"context":"all_turns"},
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"]
    });
    Ok(body)
}

fn responses_lite_prefix_id(prefix: &str, visible_payload: &[u8]) -> String {
    // Codex v0.151.0 assigns UUID-v5 IDs to the two prompt-only Responses-Lite
    // items so retries retain identity. Finch has no Codex thread UUID at this
    // provider boundary, so the audited protocol revision is the stable,
    // application-owned namespace and the visible payload remains the name.
    let namespace = Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        CHATGPT_INFERENCE_PROTOCOL_REVISION.as_bytes(),
    );
    format!("{prefix}_{}", Uuid::new_v5(&namespace, visible_payload))
}

#[cfg(test)]
fn chatgpt_bindings(request: &ProviderRequest) -> Result<ToolBindingTable> {
    compile_from_definitions(
        WireProtocol::OpenAiChatGptResponsesLite,
        "chatgpt_subscription",
        &request.model,
        request.tools.as_deref().unwrap_or_default(),
        request.tool_policy(),
    )
    .map_err(map_chatgpt_compile_error)
}

#[cfg(test)]
fn map_chatgpt_compile_error(error: ToolBindingError) -> anyhow::Error {
    match &error {
        ToolBindingError::ReservedNameCollision(_) => {
            anyhow::anyhow!("ChatGPT subscription request used a reserved wire tool name")
        }
        ToolBindingError::DuplicateLocalIdentity(_) => {
            anyhow::anyhow!("ChatGPT subscription request repeated a tool name")
        }
        ToolBindingError::DuplicateWireIdentity { .. } => {
            anyhow::anyhow!("ChatGPT subscription request repeated a wire tool name")
        }
        ToolBindingError::TooManyTools(_) => {
            anyhow::anyhow!("ChatGPT subscription request advertised too many tools")
        }
        ToolBindingError::InvalidIdentifier(_) | ToolBindingError::NameTooLong(_, _) => {
            anyhow::anyhow!("ChatGPT subscription tool name was invalid")
        }
        other => anyhow::Error::msg(other.to_string()),
    }
}

fn map_tools(bindings: &ToolBindingTable) -> Result<Vec<Value>> {
    let mut functions = Vec::with_capacity(bindings.len());
    for tool in bindings.entries() {
        validate_bounded_text(
            &tool.description,
            MAX_TOOL_ARGUMENT_BYTES,
            "tool description",
        )?;
        functions.push(tool.chatgpt_function());
    }
    Ok(if functions.is_empty() {
        Vec::new()
    } else {
        vec![json!({"type":"namespace","name":"functions","description":"","tools":functions})]
    })
}

#[cfg(test)]
pub(super) fn test_tool_bindings(names: &[&str]) -> ToolBindingTable {
    let definitions = names
        .iter()
        .map(|name| ToolDefinition {
            name: (*name).to_string(),
            description: (*name).to_string(),
            input_schema: crate::ToolInputSchema::simple(vec![]),
        })
        .collect::<Vec<_>>();
    compile_from_definitions(
        WireProtocol::OpenAiChatGptResponsesLite,
        "chatgpt_subscription",
        DEFAULT_MODEL,
        &definitions,
        &Default::default(),
    )
    .expect("test tool bindings")
}

#[cfg(test)]
pub(super) fn empty_tool_bindings() -> ToolBindingTable {
    ToolBindingTable::empty(
        WireProtocol::OpenAiChatGptResponsesLite,
        "chatgpt_subscription",
        DEFAULT_MODEL,
    )
}

fn decode_chatgpt_function<'a>(
    bindings: &'a ToolBindingTable,
    wire_name: &str,
    namespace: Option<&str>,
) -> Result<&'a crate::tool_bindings::BoundTool> {
    match bindings.decode_wire_call(wire_name, namespace) {
        Ok(bound) => Ok(bound),
        Err(ToolBindingError::ReservedNameCollision(_)) => {
            bail!("ChatGPT requested a reserved native function name")
        }
        Err(ToolBindingError::UnknownNamespace { .. }) => {
            bail!("ChatGPT function call namespace was invalid")
        }
        Err(ToolBindingError::UnknownWireCall { .. })
        | Err(ToolBindingError::UnknownSemanticIdentity(_)) => {
            bail!("ChatGPT requested a function Finch did not advertise")
        }
        Err(error) => Err(anyhow::Error::msg(error.to_string())),
    }
}

fn map_message(
    message: &Message,
    input: &mut Vec<Value>,
    calls: &mut HashSet<String>,
    results: &mut HashSet<String>,
    allowed_tools: &ToolBindingTable,
) -> Result<()> {
    if !matches!(message.role.as_str(), "user" | "assistant") {
        bail!("ChatGPT subscription history contained an unsupported role");
    }
    let mut message_content = Vec::new();
    let flush = |content: &mut Vec<Value>, input: &mut Vec<Value>| {
        if !content.is_empty() {
            input.push(
                json!({"type":"message","role":message.role,"content":std::mem::take(content)}),
            );
        }
    };
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                validate_bounded_text(text, MAX_RESPONSE_BYTES, "message text")?;
                message_content.push(json!({
                    "type": if message.role == "assistant" {"output_text"} else {"input_text"},
                    "text":text
                }));
            }
            ContentBlock::Image { source } => {
                if message.role != "user" {
                    bail!("ChatGPT subscription image input must have user role");
                }
                let image_url = super::openai::validated_image_data_url(source)
                    .context("ChatGPT subscription image input was invalid")?;
                message_content.push(json!({"type":"input_image","image_url":image_url}));
            }
            ContentBlock::OpaqueReasoning { encrypted_content } => {
                flush(&mut message_content, input);
                if message.role != "assistant" {
                    bail!("ChatGPT opaque reasoning continuation must have assistant role");
                }
                validate_bounded_text(
                    encrypted_content,
                    MAX_OPAQUE_REASONING_BYTES,
                    "opaque reasoning",
                )?;
                input.push(json!({
                    "type":"reasoning","summary":[],"encrypted_content":encrypted_content
                }));
            }
            ContentBlock::ToolUse {
                id,
                name,
                input: arguments,
            } => {
                flush(&mut message_content, input);
                if message.role != "assistant" {
                    bail!("ChatGPT function calls must have assistant role");
                }
                validate_identifier(id, 256, "tool call identifier")?;
                validate_identifier(name, 128, "tool name")?;
                let bound = allowed_tools.encode_semantic(name).map_err(|_| {
                    anyhow::anyhow!("ChatGPT subscription history used an unadvertised tool")
                })?;
                if !calls.insert(id.clone()) {
                    bail!("ChatGPT subscription history repeated a tool call identifier");
                }
                let arguments = serde_json::to_string(arguments)
                    .context("Failed to serialize ChatGPT function arguments")?;
                validate_bounded_text(&arguments, MAX_TOOL_ARGUMENT_BYTES, "tool arguments")?;
                input.push(json!({
                    "type":"function_call","call_id":id,"name":bound.wire.name,
                    "namespace":bound.wire.namespace.as_deref().unwrap_or("functions"),
                    "arguments":arguments
                }));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                flush(&mut message_content, input);
                if message.role != "user" {
                    bail!("ChatGPT function results must have user role");
                }
                validate_identifier(tool_use_id, 256, "tool call identifier")?;
                validate_bounded_text(content, MAX_TOOL_ARGUMENT_BYTES, "tool result")?;
                if !calls.contains(tool_use_id) {
                    bail!("ChatGPT subscription history contained an unmatched tool result");
                }
                if !results.insert(tool_use_id.clone()) {
                    bail!("ChatGPT subscription history repeated a tool result identifier");
                }
                input.push(json!({
                    "type":"function_call_output","call_id":tool_use_id,"output":content
                }));
            }
        }
    }
    flush(&mut message_content, input);
    Ok(())
}

fn parse_catalog(body: &[u8]) -> Result<Catalog> {
    let root: Value = serde_json::from_slice(body)
        .context("ChatGPT subscription model catalog contract changed")?;
    exact_keys(
        root.as_object()
            .context("ChatGPT catalog root was invalid")?,
        &["models"],
        "model catalog",
    )?;
    let models = root["models"]
        .as_array()
        .context("ChatGPT catalog omitted models")?;
    if models.is_empty() {
        return Err(SubscriptionCatalogUnavailable.into());
    }
    if models.len() > 512 {
        bail!("ChatGPT subscription model catalog was excessive");
    }
    let mut parsed = BTreeMap::new();
    for model in models {
        let object = model
            .as_object()
            .context("ChatGPT catalog model was invalid")?;
        let slug = required_identifier(object, "slug", 256)?;
        if !matches!(slug.as_str(), DEFAULT_MODEL | MODEL_ALIAS) {
            continue;
        }
        let supported = object
            .get("supported_in_api")
            .and_then(Value::as_bool)
            .context("ChatGPT catalog model omitted API support")?;
        let responses_lite = object
            .get("use_responses_lite")
            .and_then(Value::as_bool)
            .context("ChatGPT catalog model omitted Responses-Lite compatibility")?;
        let modalities = object
            .get("input_modalities")
            .and_then(Value::as_array)
            .context("ChatGPT catalog model omitted input modalities")?;
        let image_input = modalities
            .iter()
            .any(|value| value.as_str() == Some("image"));
        if !modalities
            .iter()
            .any(|value| value.as_str() == Some("text"))
        {
            continue;
        }
        let context_window = parse_catalog_context_window(object)?;
        if supported {
            if !responses_lite || !image_input {
                bail!("ChatGPT GPT-5.6 Sol catalog capabilities drifted");
            }
            parsed.insert(
                slug.clone(),
                CatalogModel {
                    slug,
                    context_window,
                    image_input,
                    responses_lite,
                },
            );
        }
    }
    if parsed.is_empty() {
        return Err(SubscriptionCatalogNoSelectableModel.into());
    }
    Ok(Catalog { models: parsed })
}

fn parse_catalog_context_window(object: &Map<String, Value>) -> Result<usize> {
    let value = object
        .get("context_window")
        .and_then(Value::as_u64)
        .ok_or(SubscriptionCatalogContextWindowInvalid::MissingOrMalformed)?;
    if value == 0 || value > MAX_CATALOG_CONTEXT_WINDOW {
        return Err(SubscriptionCatalogContextWindowInvalid::OutOfBounds(value).into());
    }
    usize::try_from(value)
        .map_err(|_| SubscriptionCatalogContextWindowInvalid::OutOfBounds(value).into())
}

#[derive(Default)]
struct CompletedResponse {
    id: String,
    model: String,
    blocks: Vec<ContentBlock>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    allowance: Option<Allowance>,
}

#[derive(Clone, Debug, PartialEq)]
struct Allowance {
    primary_used_percent: Option<f32>,
    secondary_used_percent: Option<f32>,
}

#[derive(Default)]
struct StreamAccumulator {
    output_items: BTreeMap<u64, Value>,
    actual_model: Option<String>,
}

impl StreamAccumulator {
    fn observe_model(&mut self, model: &str) -> Result<()> {
        validate_identifier(model, 256, "actual model")?;
        if self
            .actual_model
            .as_deref()
            .is_some_and(|observed| observed != model)
        {
            bail!("ChatGPT subscription actual model changed during the response");
        }
        self.actual_model = Some(model.to_string());
        Ok(())
    }
}

async fn consume_sse(
    response: Response,
    sender: Option<mpsc::Sender<Result<StreamChunk>>>,
    cancel: CancellationToken,
    expected_model: String,
    allowed_tools: ToolBindingTable,
    #[cfg(test)] stream_producer_observer: Option<StreamProducerObserver>,
) -> Result<CompletedResponse> {
    let header_allowance = parse_allowance_headers(response.headers())?;
    let mut accumulator = StreamAccumulator::default();
    observe_outer_model_headers(response.headers(), &mut accumulator)?;
    let header_model = accumulator.actual_model.clone();
    let mut bytes = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut total = 0usize;
    let mut terminal: Option<CompletedResponse> = None;
    let mut done_seen = false;
    let mut last_sequence = None;
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => bail!("ChatGPT subscription stream was cancelled"),
            _ = async { if let Some(sender) = sender.as_ref() { sender.closed().await } else { futures::future::pending().await } } => return Err(anyhow::anyhow!("ChatGPT subscription stream receiver was dropped")),
            next = tokio::time::timeout(STREAM_IDLE_TIMEOUT, bytes.next()) => next.context("ChatGPT subscription stream timed out")?,
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.context("ChatGPT subscription stream failed")?;
        total = total.saturating_add(chunk.len());
        if total > MAX_RESPONSE_BYTES {
            bail!("ChatGPT subscription stream exceeded the size limit");
        }
        buffer.extend_from_slice(&chunk);
        enforce_sse_remainder_bounds(&buffer)?;
        while let Some((end, separator)) = find_event_end(&buffer) {
            if end > MAX_SSE_EVENT_BYTES {
                bail!("ChatGPT subscription stream event exceeded the size limit");
            }
            let event = buffer.drain(..end).collect::<Vec<_>>();
            buffer.drain(..separator);
            let (event_name, data) = sse_data(&event)?;
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                if terminal.is_none() || done_seen {
                    bail!("ChatGPT subscription stream terminal marker was invalid");
                }
                done_seen = true;
                continue;
            }
            if terminal.is_some() || done_seen {
                bail!("ChatGPT subscription sent data after its terminal response");
            }
            let event: Value = serde_json::from_str(&data)
                .context("ChatGPT subscription stream event was malformed")?;
            let event_kind = event
                .as_object()
                .and_then(|object| object.get("type"))
                .and_then(Value::as_str)
                .context("ChatGPT subscription stream event omitted type")?;
            let text_delta = (event_kind == "response.output_text.delta")
                .then(|| {
                    event
                        .as_object()
                        .and_then(|object| object.get("delta"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .flatten();
            if event_name.as_deref().is_some_and(|name| name != event_kind) {
                bail!("ChatGPT subscription SSE event name did not match its payload");
            }
            let sequence = event
                .as_object()
                .and_then(|object| object.get("sequence_number"))
                .and_then(Value::as_u64)
                .context("ChatGPT subscription stream event omitted sequence_number")?;
            if last_sequence.is_some_and(|previous| sequence <= previous) {
                bail!("ChatGPT subscription stream sequence was not strictly increasing");
            }
            last_sequence = Some(sequence);
            if let Some(completed) = parse_event(
                event,
                &expected_model,
                header_model.as_deref(),
                &allowed_tools,
                &mut accumulator,
            )? {
                terminal = Some(completed);
            }
            if let (Some(sender), Some(delta)) = (sender.as_ref(), text_delta) {
                send_stream_chunk(
                    sender,
                    &cancel,
                    StreamChunk::TextDelta(delta),
                    #[cfg(test)]
                    stream_producer_observer.as_ref(),
                )
                .await?;
            }
            enforce_sse_remainder_bounds(&buffer)?;
        }
    }
    if !buffer.iter().all(u8::is_ascii_whitespace) {
        bail!("ChatGPT subscription stream ended with a partial event");
    }
    let mut completed =
        terminal.context("ChatGPT subscription stream ended before response.completed")?;
    completed.allowance = completed.allowance.or(header_allowance);
    if let Some(sender) = sender {
        send_stream_chunk(
            &sender,
            &cancel,
            StreamChunk::ResponseMetadata {
                model: completed.model.clone(),
            },
            #[cfg(test)]
            stream_producer_observer.as_ref(),
        )
        .await?;
        if let Some(input_tokens) = completed.input_tokens {
            send_stream_chunk(
                &sender,
                &cancel,
                StreamChunk::Usage {
                    input_tokens,
                    output_tokens: completed.output_tokens.unwrap_or_default(),
                },
                #[cfg(test)]
                stream_producer_observer.as_ref(),
            )
            .await?;
        }
        if let Some(allowance) = completed.allowance.as_ref() {
            send_stream_chunk(
                &sender,
                &cancel,
                StreamChunk::Allowance {
                    primary_used_percent: allowance.primary_used_percent,
                    secondary_used_percent: allowance.secondary_used_percent,
                },
                #[cfg(test)]
                stream_producer_observer.as_ref(),
            )
            .await?;
        }
        for block in completed.blocks.iter().cloned() {
            send_stream_chunk(
                &sender,
                &cancel,
                StreamChunk::ContentBlockComplete(block),
                #[cfg(test)]
                stream_producer_observer.as_ref(),
            )
            .await?;
        }
    }
    Ok(completed)
}

async fn send_stream_chunk(
    sender: &mpsc::Sender<Result<StreamChunk>>,
    cancel: &CancellationToken,
    chunk: StreamChunk,
    #[cfg(test)] stream_producer_observer: Option<&StreamProducerObserver>,
) -> Result<()> {
    #[cfg(test)]
    if let Some(observer) = stream_producer_observer {
        observer(StreamProducerEvent::SendAttempt(chunk.clone()));
    }
    tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("ChatGPT subscription stream was cancelled"),
        _ = sender.closed() => bail!("ChatGPT subscription stream receiver was dropped"),
        result = sender.send(Ok(chunk)) => result
            .map_err(|_| anyhow::anyhow!("ChatGPT subscription stream receiver was dropped")),
    }
}

fn parse_event(
    event: Value,
    expected_model: &str,
    header_model: Option<&str>,
    allowed_tools: &ToolBindingTable,
    accumulator: &mut StreamAccumulator,
) -> Result<Option<CompletedResponse>> {
    let object = event
        .as_object()
        .context("ChatGPT subscription stream event was not an object")?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .context("ChatGPT subscription stream event omitted type")?;
    match kind {
        "response.created" | "response.in_progress" => {
            exact_event_keys(
                object,
                &["type", "sequence_number", "response", "headers"],
                "response lifecycle event",
            )?;
            required_sequence(object)?;
            let response = object
                .get("response")
                .and_then(Value::as_object)
                .context("ChatGPT response lifecycle event omitted response")?;
            observe_response_model(response, accumulator)?;
            observe_event_headers(object, accumulator)?;
            Ok(None)
        }
        "response.metadata" | "codex.response.metadata" => {
            exact_event_keys(
                object,
                &[
                    "type",
                    "sequence_number",
                    "response_id",
                    "headers",
                    "metadata",
                    "safety_buffering",
                ],
                "response metadata event",
            )?;
            required_sequence(object)?;
            if let Some(response_id) = object.get("response_id") {
                let response_id = response_id
                    .as_str()
                    .context("ChatGPT response metadata identifier was invalid")?;
                validate_identifier(response_id, 256, "response metadata identifier")?;
            }
            for field in ["metadata", "safety_buffering"] {
                if let Some(value) = object.get(field) {
                    let encoded = serde_json::to_vec(value)
                        .context("ChatGPT response metadata was invalid")?;
                    if encoded.len() > MAX_SSE_EVENT_BYTES {
                        bail!("ChatGPT response metadata exceeded the size limit");
                    }
                }
            }
            observe_event_headers(object, accumulator)?;
            Ok(None)
        }
        "response.output_item.added" | "response.output_item.done" => {
            exact_event_keys(
                object,
                &["type", "sequence_number", "output_index", "item"],
                "response output item event",
            )?;
            required_sequence(object)?;
            let output_index = required_index(object, "output_index")?;
            let item = object
                .get("item")
                .context("ChatGPT output item event omitted item")?;
            validate_output_item_shape(item)?;
            if kind == "response.output_item.done"
                && accumulator
                    .output_items
                    .insert(output_index, item.clone())
                    .is_some()
            {
                bail!("ChatGPT subscription repeated a completed output index");
            }
            Ok(None)
        }
        "response.content_part.added" | "response.content_part.done" => {
            exact_event_keys(
                object,
                &[
                    "type",
                    "sequence_number",
                    "item_id",
                    "output_index",
                    "content_index",
                    "part",
                ],
                "response content part event",
            )?;
            required_sequence(object)?;
            required_identifier(object, "item_id", 256)?;
            required_index(object, "output_index")?;
            required_index(object, "content_index")?;
            object
                .get("part")
                .and_then(Value::as_object)
                .context("ChatGPT content event omitted part")?;
            Ok(None)
        }
        "response.output_text.delta" => {
            validate_text_event(object, "delta", true, "response output text event")?;
            Ok(None)
        }
        "response.output_text.done" => {
            validate_text_event(object, "text", true, "response output text event")?;
            Ok(None)
        }
        "response.function_call_arguments.delta" => {
            validate_text_event(
                object,
                "delta",
                false,
                "response function call arguments event",
            )?;
            Ok(None)
        }
        "response.function_call_arguments.done" => {
            validate_text_event(
                object,
                "arguments",
                false,
                "response function call arguments event",
            )?;
            Ok(None)
        }
        "response.reasoning_summary_text.delta" => {
            validate_reasoning_text_event(
                object,
                "delta",
                "summary_index",
                "response reasoning summary text event",
            )?;
            Ok(None)
        }
        "response.reasoning_summary_text.done" => {
            validate_reasoning_text_event(
                object,
                "text",
                "summary_index",
                "response reasoning summary text event",
            )?;
            Ok(None)
        }
        "response.reasoning_text.delta" => {
            validate_reasoning_text_event(
                object,
                "delta",
                "content_index",
                "response reasoning text event",
            )?;
            Ok(None)
        }
        "response.reasoning_summary_part.added" | "response.reasoning_summary_part.done" => {
            exact_event_keys(
                object,
                &[
                    "type",
                    "sequence_number",
                    "item_id",
                    "output_index",
                    "summary_index",
                    "part",
                ],
                "response reasoning summary part event",
            )?;
            required_sequence(object)?;
            required_identifier(object, "item_id", 256)?;
            required_index(object, "output_index")?;
            required_index(object, "summary_index")?;
            object
                .get("part")
                .and_then(Value::as_object)
                .context("ChatGPT reasoning summary part was invalid")?;
            Ok(None)
        }
        "response.completed" => {
            exact_event_keys(
                object,
                &["type", "sequence_number", "response"],
                "response completed event",
            )?;
            required_sequence(object)?;
            let response = object
                .get("response")
                .and_then(Value::as_object)
                .context("ChatGPT completion omitted response")?;
            parse_completed(
                response,
                expected_model,
                header_model,
                allowed_tools,
                accumulator,
            )
            .map(Some)
        }
        "response.failed" | "response.incomplete" => {
            bail!("ChatGPT subscription response failed before completion")
        }
        _ => bail!("ChatGPT subscription stream contained an unknown event type"),
    }
}

fn validate_output_item_shape(item: &Value) -> Result<()> {
    let object = item
        .as_object()
        .context("ChatGPT output item event was invalid")?;
    match object.get("type").and_then(Value::as_str) {
        Some("message") | Some("reasoning") | Some("function_call") => Ok(()),
        _ => bail!("ChatGPT output item event contained an unknown item type"),
    }
}

fn observe_response_model(
    response: &Map<String, Value>,
    accumulator: &mut StreamAccumulator,
) -> Result<()> {
    if let Some(headers) = response.get("headers") {
        let headers = headers
            .as_object()
            .context("ChatGPT response model headers were invalid")?;
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("openai-model")
                || name.eq_ignore_ascii_case("x-openai-model")
            {
                let model = match value {
                    Value::String(model) => model.as_str(),
                    Value::Array(values) if values.len() == 1 => values[0]
                        .as_str()
                        .context("ChatGPT response model header was invalid")?,
                    _ => bail!("ChatGPT response model header was invalid"),
                };
                accumulator.observe_model(model)?;
            }
        }
    }
    Ok(())
}

fn observe_event_headers(
    event: &Map<String, Value>,
    accumulator: &mut StreamAccumulator,
) -> Result<()> {
    let Some(headers) = event.get("headers") else {
        return Ok(());
    };
    let headers = headers
        .as_object()
        .context("ChatGPT response model headers were invalid")?;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("openai-model") || name.eq_ignore_ascii_case("x-openai-model")
        {
            let model = match value {
                Value::String(model) => model.as_str(),
                Value::Array(values) if values.len() == 1 => values[0]
                    .as_str()
                    .context("ChatGPT response model header was invalid")?,
                _ => bail!("ChatGPT response model header was invalid"),
            };
            accumulator.observe_model(model)?;
        }
    }
    Ok(())
}

fn required_sequence(object: &Map<String, Value>) -> Result<u64> {
    object
        .get("sequence_number")
        .and_then(Value::as_u64)
        .context("ChatGPT stream event omitted a valid sequence number")
}

fn required_index(object: &Map<String, Value>, name: &str) -> Result<u64> {
    object
        .get(name)
        .and_then(Value::as_u64)
        .context("ChatGPT stream event omitted a valid output index")
}

fn validate_text_event(
    object: &Map<String, Value>,
    field: &str,
    has_content_index: bool,
    location: &'static str,
) -> Result<()> {
    let mut keys = vec!["type", "sequence_number", "item_id", "output_index", field];
    if has_content_index {
        keys.extend(["content_index", "logprobs"]);
    }
    exact_event_keys(object, &keys, location)?;
    required_sequence(object)?;
    required_identifier(object, "item_id", 256)?;
    required_index(object, "output_index")?;
    if has_content_index {
        required_index(object, "content_index")?;
    }
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .context("ChatGPT stream delta omitted its text")?;
    validate_bounded_text(value, MAX_TOOL_ARGUMENT_BYTES, "stream delta")
}

fn validate_reasoning_text_event(
    object: &Map<String, Value>,
    field: &str,
    index_field: &str,
    location: &'static str,
) -> Result<()> {
    exact_event_keys(
        object,
        &[
            "type",
            "sequence_number",
            "item_id",
            "output_index",
            index_field,
            field,
        ],
        location,
    )?;
    required_sequence(object)?;
    required_identifier(object, "item_id", 256)?;
    required_index(object, "output_index")?;
    required_index(object, index_field)?;
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .context("ChatGPT reasoning stream event omitted text")?;
    validate_bounded_text(value, MAX_TOOL_ARGUMENT_BYTES, "reasoning stream text")
}

fn parse_completed(
    response: &Map<String, Value>,
    expected_model: &str,
    header_model: Option<&str>,
    allowed_tools: &ToolBindingTable,
    accumulator: &mut StreamAccumulator,
) -> Result<CompletedResponse> {
    exact_keys(
        response,
        &[
            "id",
            "object",
            "created_at",
            "status",
            "error",
            "incomplete_details",
            "instructions",
            "max_output_tokens",
            "model",
            "output",
            "parallel_tool_calls",
            "previous_response_id",
            "reasoning",
            "store",
            "temperature",
            "text",
            "tool_choice",
            "tools",
            "top_p",
            "truncation",
            "usage",
            "user",
            "metadata",
            "service_tier",
            "prompt_cache_key",
            "safety_identifier",
            "headers",
            "usage_metadata",
            "end_turn",
            "background",
            "completed_at",
            "conversation",
            "max_tool_calls",
            "moderation",
            "prompt",
            "prompt_cache_diagnostics",
            "prompt_cache_options",
            "prompt_cache_retention",
            "top_logprobs",
            "frequency_penalty",
            "presence_penalty",
            "tool_usage",
        ],
        "terminal response",
    )?;
    if let Some(tool_usage) = response.get("tool_usage") {
        if serde_json::to_vec(tool_usage)
            .context("ChatGPT terminal response tool usage metadata was invalid")?
            .len()
            > MAX_TOOL_ARGUMENT_BYTES
        {
            bail!("ChatGPT terminal response tool usage metadata exceeded the size limit");
        }
    }
    validate_documented_response_metadata(response)?;
    if response
        .get("status")
        .is_some_and(|status| status.as_str() != Some("completed"))
    {
        bail!("ChatGPT terminal response status was invalid");
    }
    let id = required_identifier(response, "id", 256)?;
    observe_response_model(response, accumulator)?;
    if let Some(header_model) = header_model {
        accumulator.observe_model(header_model)?;
    }
    // Responses-Lite may route the selected catalog model through another
    // serving model and does not consistently emit an `openai-model` header.
    // Report a bounded explicit serving model when present; otherwise retain
    // the validated route Finch requested. Never trust the terminal payload's
    // passive `model` field, and reject contradictory explicit identities.
    let response_model = accumulator
        .actual_model
        .clone()
        .unwrap_or_else(|| expected_model.to_string());
    let terminal_output = response
        .get("output")
        .map(|output| {
            output
                .as_array()
                .context("ChatGPT completion output items were invalid")
        })
        .transpose()?;
    if terminal_output.is_some_and(|output| output.len() > MAX_OUTPUT_ITEMS)
        || accumulator.output_items.len() > MAX_OUTPUT_ITEMS
    {
        bail!("ChatGPT completion returned too many output items");
    }
    if let Some(output) = terminal_output {
        if !accumulator.output_items.is_empty() {
            validate_output_snapshot(accumulator.output_items.values(), allowed_tools)?;
            // An explicit empty completion `output` has the same meaning as an
            // omitted snapshot: Responses-Lite supplied no redundant terminal
            // projection, so the validated `response.output_item.done` stream
            // remains authoritative. A non-empty snapshot must still reconcile
            // in order and one-to-one so it cannot conceal message or tool drift.
        }
        if !accumulator.output_items.is_empty() && !output.is_empty() {
            validate_output_snapshot(output.iter(), allowed_tools)?;
            let streamed_items = accumulator
                .output_items
                .values()
                .map(canonical_output_item)
                .collect::<Vec<_>>();
            let streamed_values = accumulator.output_items.values().collect::<Vec<_>>();
            let mut streamed_index = 0;
            for (terminal_index, item) in output.iter().enumerate() {
                let terminal_item = canonical_output_item(item);
                while streamed_index < streamed_items.len()
                    && streamed_items[streamed_index] != terminal_item
                    && output_item_kind(streamed_values[streamed_index]) == "reasoning"
                {
                    streamed_index += 1;
                }
                if streamed_index >= streamed_items.len() {
                    bail!(
                        "ChatGPT terminal output item {terminal_index} ({terminal_kind}) did not \
                         match a remaining streamed semantic item; terminal_count={}, \
                         streamed_count={}",
                        output.len(),
                        streamed_items.len(),
                        terminal_kind = output_item_kind(item),
                    );
                }
                if streamed_items[streamed_index] != terminal_item {
                    bail!(
                        "ChatGPT terminal output item {terminal_index} ({terminal_kind}) did not \
                         match streamed output item {streamed_index} ({streamed_kind}); \
                         terminal_count={}, streamed_count={}",
                        output.len(),
                        streamed_items.len(),
                        terminal_kind = output_item_kind(item),
                        streamed_kind = output_item_kind(streamed_values[streamed_index]),
                    );
                }
                streamed_index += 1;
            }
            while streamed_index < streamed_items.len()
                && output_item_kind(streamed_values[streamed_index]) == "reasoning"
            {
                streamed_index += 1;
            }
            if streamed_index < streamed_items.len() {
                bail!(
                    "ChatGPT terminal snapshot omitted streamed output item {streamed_index} \
                     ({streamed_kind}); terminal_count={}, streamed_count={}",
                    output.len(),
                    streamed_items.len(),
                    streamed_kind = output_item_kind(streamed_values[streamed_index]),
                );
            }
        }
        if accumulator.output_items.is_empty() {
            accumulator.output_items = output
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, item)| (index as u64, item))
                .collect();
        }
    }
    if accumulator
        .output_items
        .keys()
        .copied()
        .ne(0..accumulator.output_items.len() as u64)
    {
        bail!("ChatGPT response output indices were not contiguous");
    }
    let mut blocks = Vec::new();
    let mut call_ids = HashSet::new();
    for item in accumulator.output_items.values() {
        parse_output_item(item, &mut blocks, &mut call_ids, allowed_tools)?;
    }
    let (input_tokens, output_tokens) = parse_usage(response.get("usage"))?;
    Ok(CompletedResponse {
        id,
        model: response_model,
        blocks,
        input_tokens,
        output_tokens,
        allowance: None,
    })
}

fn validate_output_snapshot<'a>(
    items: impl Iterator<Item = &'a Value>,
    allowed_tools: &ToolBindingTable,
) -> Result<()> {
    let mut blocks = Vec::new();
    let mut call_ids = HashSet::new();
    for item in items {
        parse_output_item(item, &mut blocks, &mut call_ids, allowed_tools)?;
    }
    Ok(())
}

fn canonical_output_item(item: &Value) -> Value {
    let mut canonical = item.clone();
    let Some(object) = canonical.as_object_mut() else {
        return canonical;
    };
    for field in [
        "id",
        "status",
        "phase",
        "internal_chat_message_metadata_passthrough",
    ] {
        object.remove(field);
    }
    if object.get("type").and_then(Value::as_str) == Some("message") {
        if let Some(content) = object.get_mut("content").and_then(Value::as_array_mut) {
            for part in content {
                let Some(part) = part.as_object_mut() else {
                    continue;
                };
                if part.get("type").and_then(Value::as_str) == Some("output_text") {
                    part.remove("annotations");
                    part.remove("logprobs");
                }
            }
        }
    }
    if object.get("type").and_then(Value::as_str) == Some("function_call") {
        // Validation has already established that omitted `functions` and the
        // aliased `collaboration` spelling denote the same advertised custom
        // function. Compare that meaning, not the provider's projection form.
        object.insert("namespace".to_string(), json!("functions"));
    }
    canonical
}

fn output_item_kind(item: &Value) -> &str {
    item.as_object()
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("invalid")
}

fn parse_output_item(
    item: &Value,
    blocks: &mut Vec<ContentBlock>,
    call_ids: &mut HashSet<String>,
    allowed_tools: &ToolBindingTable,
) -> Result<()> {
    let object = item
        .as_object()
        .context("ChatGPT response output item was invalid")?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .context("ChatGPT response output item omitted type")?;
    match kind {
        "message" => {
            exact_keys(
                object,
                &[
                    "id",
                    "type",
                    "status",
                    "role",
                    "content",
                    "phase",
                    "internal_chat_message_metadata_passthrough",
                ],
                "message output item",
            )?;
            if object.get("role").and_then(Value::as_str) != Some("assistant") {
                bail!("ChatGPT response message role was invalid");
            }
            validate_optional_item_fields(object)?;
            if object
                .get("phase")
                .is_some_and(|phase| !matches!(phase.as_str(), Some("commentary" | "final_answer")))
            {
                bail!("ChatGPT response message phase was invalid");
            }
            let content = object
                .get("content")
                .and_then(Value::as_array)
                .context("ChatGPT response message omitted content")?;
            for part in content {
                let part = part
                    .as_object()
                    .context("ChatGPT response content was invalid")?;
                exact_keys(
                    part,
                    &["type", "text", "annotations", "logprobs"],
                    "output text content",
                )?;
                if part.get("type").and_then(Value::as_str) != Some("output_text") {
                    bail!("ChatGPT response contained an unknown message content type");
                }
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .context("ChatGPT output text was invalid")?;
                validate_bounded_text(text, MAX_RESPONSE_BYTES, "output text")?;
                for field in ["annotations", "logprobs"] {
                    if let Some(value) = part.get(field) {
                        let values = value
                            .as_array()
                            .context("ChatGPT output text metadata was invalid")?;
                        if values.len() > 256
                            || serde_json::to_vec(values)
                                .context("ChatGPT output text metadata was invalid")?
                                .len()
                                > MAX_TOOL_ARGUMENT_BYTES
                        {
                            bail!("ChatGPT output text metadata exceeded the size limit");
                        }
                    }
                }
                blocks.push(ContentBlock::Text {
                    text: text.to_string(),
                });
            }
        }
        "reasoning" => {
            exact_keys(
                object,
                &[
                    "id",
                    "type",
                    "summary",
                    "content",
                    "encrypted_content",
                    "status",
                    "internal_chat_message_metadata_passthrough",
                ],
                "reasoning output item",
            )?;
            validate_optional_item_fields(object)?;
            validate_reasoning_projection(object.get("summary"), "reasoning summary")?;
            validate_reasoning_projection(object.get("content"), "reasoning content")?;
            let encrypted = object
                .get("encrypted_content")
                .and_then(Value::as_str)
                .context("ChatGPT reasoning item omitted encrypted continuation")?;
            validate_bounded_text(encrypted, MAX_OPAQUE_REASONING_BYTES, "opaque reasoning")?;
            blocks.push(ContentBlock::opaque_reasoning(encrypted));
        }
        "function_call" => {
            exact_keys(
                object,
                &[
                    "id",
                    "type",
                    "status",
                    "call_id",
                    "name",
                    "arguments",
                    "namespace",
                    "encrypted_function_args",
                    "internal_chat_message_metadata_passthrough",
                ],
                "function call output item",
            )?;
            validate_optional_item_fields(object)?;
            let call_id = required_identifier(object, "call_id", 256)?;
            let wire_name = required_identifier(object, "name", 128)?;
            let namespace = object
                .get("namespace")
                .map(|namespace| {
                    namespace
                        .as_str()
                        .context("ChatGPT function call namespace was invalid")
                })
                .transpose()?;
            let bound = decode_chatgpt_function(allowed_tools, &wire_name, namespace)?;
            let name = bound.semantic.as_str();
            let arguments = object
                .get("arguments")
                .and_then(Value::as_str)
                .context("ChatGPT function call omitted arguments")?;
            validate_bounded_text(arguments, MAX_TOOL_ARGUMENT_BYTES, "tool arguments")?;
            if let Some(encrypted) = object.get("encrypted_function_args") {
                let encrypted = encrypted
                    .as_array()
                    .context("ChatGPT encrypted function arguments were invalid")?;
                if encrypted.len() > 64 {
                    bail!("ChatGPT encrypted function arguments were excessive");
                }
                for value in encrypted {
                    let value = value
                        .as_str()
                        .context("ChatGPT encrypted function arguments were invalid")?;
                    validate_bounded_text(
                        value,
                        MAX_TOOL_ARGUMENT_BYTES,
                        "encrypted function arguments",
                    )?;
                }
            }
            if !call_ids.insert(call_id.clone()) {
                bail!("ChatGPT response repeated a function call identifier");
            }
            let input: Value = serde_json::from_str(arguments)
                .context("ChatGPT function call arguments were malformed")?;
            if !input.is_object() {
                bail!("ChatGPT function call arguments were not an object");
            }
            blocks.push(ContentBlock::ToolUse {
                id: call_id,
                name: name.to_string(),
                input,
            });
        }
        _ => bail!("ChatGPT response contained an unknown output item type"),
    }
    Ok(())
}

fn validate_documented_response_metadata(response: &Map<String, Value>) -> Result<()> {
    let optional_number = |name: &str, minimum: f64, maximum: f64| -> Result<()> {
        let Some(value) = response.get(name) else {
            return Ok(());
        };
        if value.is_null() {
            return Ok(());
        }
        value
            .as_f64()
            .filter(|value| value.is_finite() && *value >= minimum && *value <= maximum)
            .with_context(|| format!("ChatGPT terminal response {name} was invalid"))?;
        Ok(())
    };
    optional_number("created_at", 0.0, u64::MAX as f64)?;
    optional_number("completed_at", 0.0, u64::MAX as f64)?;
    optional_number("temperature", 0.0, 2.0)?;
    optional_number("top_p", 0.0, 1.0)?;
    optional_number("frequency_penalty", -2.0, 2.0)?;
    optional_number("presence_penalty", -2.0, 2.0)?;

    if response
        .get("object")
        .is_some_and(|value| value.as_str() != Some("response"))
    {
        bail!("ChatGPT terminal response object type was invalid");
    }
    for (name, expected) in [("parallel_tool_calls", false), ("store", false)] {
        if response
            .get(name)
            .is_some_and(|value| !value.is_null() && value.as_bool() != Some(expected))
        {
            bail!("ChatGPT terminal response {name} was invalid");
        }
    }
    if response
        .get("background")
        .is_some_and(|value| !value.is_null() && value.as_bool() != Some(false))
    {
        bail!("ChatGPT terminal response background state was invalid");
    }
    if response
        .get("end_turn")
        .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
    {
        bail!("ChatGPT terminal response end_turn was invalid");
    }
    for name in ["max_output_tokens", "max_tool_calls"] {
        if response.get(name).is_some_and(|value| {
            !value.is_null() && value.as_u64().is_none_or(|value| value > u32::MAX as u64)
        }) {
            bail!("ChatGPT terminal response {name} was invalid");
        }
    }
    if response
        .get("top_logprobs")
        .is_some_and(|value| !value.is_null() && value.as_u64().is_none_or(|value| value > 20))
    {
        bail!("ChatGPT terminal response top_logprobs was invalid");
    }
    if let Some(value) = response
        .get("conversation")
        .filter(|value| !value.is_null())
    {
        let conversation = value
            .as_object()
            .context("ChatGPT terminal response conversation was invalid")?;
        exact_keys(conversation, &["id"], "terminal response conversation")?;
        required_identifier(conversation, "id", 256)?;
    }
    if let Some(value) = response
        .get("prompt_cache_options")
        .filter(|value| !value.is_null())
    {
        let options = value
            .as_object()
            .context("ChatGPT terminal response prompt cache options were invalid")?;
        exact_keys(
            options,
            &["mode", "ttl", "comparison_response_id"],
            "terminal response prompt cache options",
        )?;
        if !matches!(
            options.get("mode").and_then(Value::as_str),
            Some("implicit" | "explicit")
        ) || options.get("ttl").and_then(Value::as_str) != Some("30m")
        {
            bail!("ChatGPT terminal response prompt cache options were invalid");
        }
        if options.contains_key("comparison_response_id") {
            required_identifier(options, "comparison_response_id", 256)?;
        }
    }
    if response.get("prompt_cache_retention").is_some_and(|value| {
        !value.is_null() && !matches!(value.as_str(), Some("in_memory" | "24h"))
    }) {
        bail!("ChatGPT terminal response prompt cache retention was invalid");
    }
    if response.get("service_tier").is_some_and(|value| {
        !value.is_null()
            && !matches!(
                value.as_str(),
                Some("auto" | "default" | "flex" | "scale" | "priority" | "fast" | "ultrafast")
            )
    }) {
        bail!("ChatGPT terminal response service tier was invalid");
    }
    for (name, maximum) in [
        ("prompt_cache_key", 256usize),
        ("safety_identifier", 64usize),
        ("user", 256usize),
    ] {
        if let Some(value) = response.get(name).filter(|value| !value.is_null()) {
            let value = value
                .as_str()
                .with_context(|| format!("ChatGPT terminal response {name} was invalid"))?;
            validate_bounded_text(value, maximum, name)?;
        }
    }
    if let Some(value) = response.get("metadata").filter(|value| !value.is_null()) {
        let metadata = value
            .as_object()
            .context("ChatGPT terminal response metadata was invalid")?;
        if metadata.len() > 16 {
            bail!("ChatGPT terminal response metadata was excessive");
        }
        for (key, value) in metadata {
            if key.len() > 64 || value.as_str().is_none_or(|value| value.len() > 512) {
                bail!("ChatGPT terminal response metadata was invalid");
            }
        }
    }
    for name in [
        "moderation",
        "prompt",
        "prompt_cache_diagnostics",
        "reasoning",
        "text",
        "headers",
        "usage_metadata",
    ] {
        if let Some(value) = response.get(name).filter(|value| !value.is_null()) {
            if !value.is_object()
                || serde_json::to_vec(value)
                    .context("ChatGPT terminal response metadata was invalid")?
                    .len()
                    > MAX_TOOL_ARGUMENT_BYTES
            {
                bail!("ChatGPT terminal response {name} was invalid");
            }
        }
    }
    Ok(())
}

fn validate_optional_item_fields(object: &Map<String, Value>) -> Result<()> {
    if let Some(id) = object.get("id") {
        let id = id
            .as_str()
            .context("ChatGPT response output item identifier was invalid")?;
        validate_identifier(id, 256, "output item identifier")?;
    }
    if object
        .get("status")
        .is_some_and(|status| status.as_str() != Some("completed"))
    {
        bail!("ChatGPT response output item status was invalid");
    }
    if let Some(metadata) = object.get("internal_chat_message_metadata_passthrough") {
        let encoded =
            serde_json::to_vec(metadata).context("ChatGPT response item metadata was invalid")?;
        if encoded.len() > MAX_TOOL_ARGUMENT_BYTES {
            bail!("ChatGPT response item metadata exceeded the size limit");
        }
    }
    Ok(())
}

fn validate_reasoning_projection(value: Option<&Value>, label: &str) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let items = value
        .as_array()
        .with_context(|| format!("ChatGPT {label} was invalid"))?;
    if items.len() > 256 {
        bail!("ChatGPT {label} was excessive");
    }
    for item in items {
        let item = item
            .as_object()
            .with_context(|| format!("ChatGPT {label} was invalid"))?;
        exact_keys(item, &["type", "text"], "reasoning projection item")?;
        if !matches!(
            item.get("type").and_then(Value::as_str),
            Some("summary_text" | "reasoning_text" | "text")
        ) {
            bail!("ChatGPT {label} contained an unknown item type");
        }
        let text = item
            .get("text")
            .and_then(Value::as_str)
            .with_context(|| format!("ChatGPT {label} text was invalid"))?;
        validate_bounded_text(text, MAX_TOOL_ARGUMENT_BYTES, label)?;
    }
    Ok(())
}

fn parse_usage(value: Option<&Value>) -> Result<(Option<u32>, Option<u32>)> {
    let Some(value) = value else {
        return Ok((None, None));
    };
    let object = value
        .as_object()
        .context("ChatGPT response usage was invalid")?;
    exact_keys(
        object,
        &[
            "input_tokens",
            "input_tokens_details",
            "output_tokens",
            "output_tokens_details",
            "total_tokens",
            "codex_rollout_budget_units",
            "extra",
            "attribution",
        ],
        "response usage",
    )?;
    if let Some(extra) = object.get("extra") {
        extra
            .as_object()
            .context("ChatGPT response usage extra metadata was invalid")?;
    }
    if let Some(attribution) = object.get("attribution") {
        attribution
            .as_object()
            .context("ChatGPT response usage attribution metadata was invalid")?;
        let encoded = serde_json::to_vec(attribution)
            .context("ChatGPT response usage attribution metadata was invalid")?;
        if encoded.len() > MAX_USAGE_METADATA_BYTES {
            bail!("ChatGPT response usage attribution metadata exceeded the size limit");
        }
    }
    let convert = |name: &str| -> Result<Option<u32>> {
        object
            .get(name)
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .context("ChatGPT response usage exceeded protocol limits")
            })
            .transpose()
    };
    let input = convert("input_tokens")?;
    let output = convert("output_tokens")?;
    if input.is_some() != output.is_some() {
        bail!("ChatGPT response returned incomplete usage metadata");
    }
    if let Some(total) = convert("total_tokens")? {
        if Some(total)
            != input
                .zip(output)
                .map(|(input, output)| input.saturating_add(output))
        {
            bail!("ChatGPT response usage totals were inconsistent");
        }
    }
    Ok((input, output))
}

fn parse_allowance_headers(headers: &reqwest::header::HeaderMap) -> Result<Option<Allowance>> {
    let parse = |name: &str| -> Result<Option<f32>> {
        bounded_header(headers, name)?
            .map(|value| {
                value
                    .parse::<f32>()
                    .ok()
                    .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
                    .context("ChatGPT allowance header was invalid")
            })
            .transpose()
    };
    let primary = parse("x-codex-primary-used-percent")?;
    let secondary = parse("x-codex-secondary-used-percent")?;
    Ok(
        (primary.is_some() || secondary.is_some()).then_some(Allowance {
            primary_used_percent: primary,
            secondary_used_percent: secondary,
        }),
    )
}

fn exact_keys(object: &Map<String, Value>, allowed: &[&str], location: &'static str) -> Result<()> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        bail!("ChatGPT subscription {location} contained an unknown field");
    }
    Ok(())
}

/// Validate an audited SSE event envelope while accepting only bounded passive
/// padding fields that cannot alter Finch's execution semantics.
fn exact_event_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    location: &'static str,
) -> Result<()> {
    if let Some(obfuscation) = object.get("obfuscation") {
        let obfuscation = obfuscation
            .as_str()
            .context("ChatGPT subscription response obfuscation padding was invalid")?;
        validate_bounded_text(
            obfuscation,
            MAX_SSE_LINE_BYTES,
            "response obfuscation padding",
        )?;
    }
    if let Some(safety_buffering) = object.get("safety_buffering") {
        let encoded = serde_json::to_vec(safety_buffering)
            .context("ChatGPT subscription safety buffering metadata was invalid")?;
        if encoded.len() > MAX_SSE_EVENT_BYTES {
            bail!("ChatGPT subscription safety buffering metadata exceeded the size limit");
        }
    }
    let mut event_fields = Vec::with_capacity(allowed.len() + 2);
    event_fields.extend_from_slice(allowed);
    event_fields.extend(["obfuscation", "safety_buffering"]);
    exact_keys(object, &event_fields, location)
}

fn required_identifier(object: &Map<String, Value>, name: &str, maximum: usize) -> Result<String> {
    let value = object
        .get(name)
        .and_then(Value::as_str)
        .context("ChatGPT subscription response omitted a required identifier")?;
    validate_identifier(value, maximum, "identifier")?;
    Ok(value.to_string())
}

fn validate_identifier(value: &str, maximum: usize, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        bail!("ChatGPT subscription {label} was invalid");
    }
    Ok(())
}

fn validate_bounded_text(value: &str, maximum: usize, label: &str) -> Result<()> {
    if value.len() > maximum {
        bail!("ChatGPT subscription {label} exceeded the size limit");
    }
    Ok(())
}

fn bounded_header(headers: &reqwest::header::HeaderMap, name: &str) -> Result<Option<String>> {
    headers
        .get(name)
        .map(|value| {
            let value = value
                .to_str()
                .context("ChatGPT subscription response header was invalid")?;
            validate_bounded_text(value, 1024, "response header")?;
            Ok(value.to_string())
        })
        .transpose()
}

fn observe_outer_model_headers(
    headers: &reqwest::header::HeaderMap,
    accumulator: &mut StreamAccumulator,
) -> Result<()> {
    for value in headers.get_all("openai-model") {
        let model = value
            .to_str()
            .context("ChatGPT subscription response model header was invalid")?;
        accumulator.observe_model(model)?;
    }
    Ok(())
}

async fn read_bounded(
    response: Response,
    maximum: usize,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => bail!("ChatGPT subscription response read was cancelled"),
            next = stream.next() => next,
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.context("ChatGPT subscription response read failed")?;
        if body.len().saturating_add(chunk.len()) > maximum {
            bail!("ChatGPT subscription response exceeded the size limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn find_event_end(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|p| (p, 2));
    let crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|p| (p, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn enforce_sse_remainder_bounds(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_SSE_EVENT_BYTES && find_event_end(bytes).is_none() {
        bail!("ChatGPT subscription stream event exceeded the size limit");
    }
    let line = bytes.rsplit(|byte| *byte == b'\n').next().unwrap_or(bytes);
    if line.len() > MAX_SSE_LINE_BYTES {
        bail!("ChatGPT subscription stream line exceeded the size limit");
    }
    Ok(())
}

fn sse_data(event: &[u8]) -> Result<(Option<String>, String)> {
    let event = std::str::from_utf8(event).context("ChatGPT subscription SSE was not UTF-8")?;
    let mut event_name = None;
    let mut data = String::new();
    for line in event.lines() {
        if line.len() > MAX_SSE_LINE_BYTES {
            bail!("ChatGPT subscription stream line exceeded the size limit");
        }
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            if event_name.is_some() {
                bail!("ChatGPT subscription SSE repeated its event name");
            }
            let value = value.trim_start();
            validate_identifier(value, 128, "SSE event name")?;
            event_name = Some(value.to_string());
            continue;
        }
        let value = line
            .strip_prefix("data:")
            .context("ChatGPT subscription SSE contained an unknown field")?;
        if !data.is_empty() {
            data.push('\n');
        }
        data.push_str(value.trim_start());
    }
    Ok((event_name, data))
}

#[cfg(test)]
mod tests;
