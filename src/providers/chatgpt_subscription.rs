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
use crate::claude::{ContentBlock, Message};
use crate::config::{
    AudienceBinding, CredentialProvider, EndpointFamily, ProviderCredential, ReasoningEffort,
};
use crate::oauth::file_store::FileOAuthCredentialStore;
use crate::oauth::{OAuthClient, OAuthCredentialStore, OAuthTokenRecord};
use crate::tools::types::ToolDefinition;

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
    OpenAiChatGptOAuthDialect<crate::providers::openai_jwks::OpenAiJwksVerifier>,
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
        })
    }

    #[cfg(test)]
    fn for_test(source: Arc<dyn ChatGptCredentialSource>, base: &str, model: &str) -> Result<Self> {
        Self::new(source, base, model, ReasoningEffort::High, true)
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
    ) -> Result<Response> {
        let body = responses_lite_request(&request, self.reasoning_effort)?;
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
        let request = request.into_request_for(self)?;
        let expected_model = request.model.clone();
        let allowed_tools = advertised_tool_names(&request);
        let response = self
            .start_response(request, CancellationToken::new())
            .await?;
        let completed = consume_sse(
            response,
            None,
            CancellationToken::new(),
            expected_model,
            allowed_tools,
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
        let request = request.into_request_for(self)?;
        let expected_model = request.model.clone();
        let allowed_tools = advertised_tool_names(&request);
        let cancel = request.cancellation_token.clone().unwrap_or_default();
        let response = self.start_response(request, cancel.clone()).await?;
        let (sender, receiver) = mpsc::channel(32);
        tokio::spawn(async move {
            let failure = sender.clone();
            if let Err(error) = consume_sse(
                response,
                Some(sender),
                cancel,
                expected_model,
                allowed_tools,
            )
            .await
            {
                let _ = failure.send(Err(anyhow::anyhow!(error.to_string()))).await;
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

fn responses_lite_request(request: &ProviderRequest, effort: ReasoningEffort) -> Result<Value> {
    validate_model(&request.model)?;
    validate_reasoning(effort)?;
    if request.temperature.is_some() {
        bail!("ChatGPT Responses-Lite does not accept Finch temperature overrides");
    }
    let tools = map_tools(request.tools.as_deref().unwrap_or_default())?;
    let allowed_tools = advertised_tool_names(request);
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
        map_message(
            message,
            &mut input,
            &mut calls,
            &mut results,
            &allowed_tools,
        )?;
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

fn map_tools(tools: &[ToolDefinition]) -> Result<Vec<Value>> {
    if tools.len() > 256 {
        bail!("ChatGPT subscription request advertised too many tools");
    }
    let mut names = BTreeSet::new();
    let mut functions = Vec::with_capacity(tools.len());
    for tool in tools {
        validate_identifier(&tool.name, 128, "tool name")?;
        validate_bounded_text(
            &tool.description,
            MAX_TOOL_ARGUMENT_BYTES,
            "tool description",
        )?;
        if !names.insert(tool.name.clone()) {
            bail!("ChatGPT subscription request repeated a tool name");
        }
        functions.push(json!({
            "type":"function",
            "name":tool.name,
            "description":tool.description,
            "strict":false,
            "parameters": {
                "type":tool.input_schema.schema_type,
                "properties":tool.input_schema.properties,
                "required":tool.input_schema.required,
                "additionalProperties":false
            }
        }));
    }
    Ok(if functions.is_empty() {
        Vec::new()
    } else {
        vec![json!({"type":"namespace","name":"functions","description":"","tools":functions})]
    })
}

fn advertised_tool_names(request: &ProviderRequest) -> HashSet<String> {
    request
        .tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|tool| tool.name.clone())
        .collect()
}

fn map_message(
    message: &Message,
    input: &mut Vec<Value>,
    calls: &mut HashSet<String>,
    results: &mut HashSet<String>,
    allowed_tools: &HashSet<String>,
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
                if !allowed_tools.contains(name) {
                    bail!("ChatGPT subscription history used an unadvertised tool");
                }
                if !calls.insert(id.clone()) {
                    bail!("ChatGPT subscription history repeated a tool call identifier");
                }
                let arguments = serde_json::to_string(arguments)
                    .context("Failed to serialize ChatGPT function arguments")?;
                validate_bounded_text(&arguments, MAX_TOOL_ARGUMENT_BYTES, "tool arguments")?;
                input.push(json!({
                    "type":"function_call","call_id":id,"name":name,
                    "namespace":"functions","arguments":arguments
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
    allowed_tools: HashSet<String>,
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
                sender
                    .send(Ok(StreamChunk::TextDelta(delta)))
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!("ChatGPT subscription stream receiver was dropped")
                    })?;
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
        sender
            .send(Ok(StreamChunk::ResponseMetadata {
                model: completed.model.clone(),
            }))
            .await
            .map_err(|_| anyhow::anyhow!("ChatGPT subscription stream receiver was dropped"))?;
        if let Some(input_tokens) = completed.input_tokens {
            sender
                .send(Ok(StreamChunk::Usage {
                    input_tokens,
                    output_tokens: completed.output_tokens.unwrap_or_default(),
                }))
                .await
                .map_err(|_| anyhow::anyhow!("ChatGPT subscription stream receiver was dropped"))?;
        }
        if let Some(allowance) = completed.allowance.as_ref() {
            sender
                .send(Ok(StreamChunk::Allowance {
                    primary_used_percent: allowance.primary_used_percent,
                    secondary_used_percent: allowance.secondary_used_percent,
                }))
                .await
                .map_err(|_| anyhow::anyhow!("ChatGPT subscription stream receiver was dropped"))?;
        }
        for block in completed.blocks.iter().cloned() {
            sender
                .send(Ok(StreamChunk::ContentBlockComplete(block)))
                .await
                .map_err(|_| anyhow::anyhow!("ChatGPT subscription stream receiver was dropped"))?;
        }
    }
    Ok(completed)
}

fn parse_event(
    event: Value,
    expected_model: &str,
    header_model: Option<&str>,
    allowed_tools: &HashSet<String>,
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
    allowed_tools: &HashSet<String>,
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
    allowed_tools: &HashSet<String>,
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
    allowed_tools: &HashSet<String>,
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
            let name = required_identifier(object, "name", 128)?;
            if object.get("namespace").and_then(Value::as_str) != Some("functions") {
                bail!("ChatGPT function call namespace was invalid");
            }
            if !allowed_tools.contains(&name) {
                bail!("ChatGPT requested a function Finch did not advertise");
            }
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
                name,
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
mod tests {
    use super::*;
    use crate::providers::LlmProvider;
    use crate::tools::types::ToolInputSchema;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const VALID_PNG_BASE64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

    fn production_credential() -> ProviderCredential {
        toml::from_str(
            r#"name = "work"
kind = "oauth_device"
provider = "chatgpt_subscription"
issuer = "openai-chatgpt"
account = "account-1"
secret_ref = "oauth-store:work"

[audience]
family = "chatgpt_subscription"
"#,
        )
        .expect("production credential fixture must deserialize")
    }

    #[tokio::test]
    async fn test_production_omitted_reasoning_uses_medium_in_request_and_debug_metadata() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(catalog_body())
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .match_body(mockito::Matcher::PartialJson(json!({
                "reasoning": {"effort": "medium", "context": "all_turns"}
            })))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_body(completed_sse(DEFAULT_MODEL))
            .create_async()
            .await;
        let mut provider = ChatGptSubscriptionProvider::production(
            &production_credential(),
            Some(DEFAULT_MODEL),
            None,
        )
        .expect("omitted reasoning must construct the production subscription provider");
        provider.source = Arc::new(StaticSource::new());
        provider.base = validate_base(&format!("{}/backend-api/codex", server.url()), true)
            .expect("loopback production-boundary fixture must have a valid base URL");
        provider.allow_loopback = true;
        let request = ProviderRequest::new(vec![Message::user("hello")])
            .with_model(DEFAULT_MODEL)
            .with_tools(vec![tool()]);

        assert_eq!(
            provider.requested_reasoning_effort(&request),
            Some(ReasoningEffort::Medium),
            "omitted subscription reasoning selected the wrong effective effort"
        );
        let debug = format!("{provider:?}");
        assert!(
            debug.contains("reasoning_effort: Medium"),
            "provider debug metadata omitted the effective reasoning effort: {debug}"
        );
        assert!(
            !debug.contains("account-1") && !debug.contains("oauth-store:work"),
            "provider debug metadata exposed credential identity: {debug}"
        );

        let response = provider
            .send_message(&request)
            .await
            .expect("production provider failed to dispatch the omitted reasoning default");
        models.assert_async().await;
        inference.assert_async().await;
        assert_eq!(
            response.text(),
            "hello",
            "production-boundary fixture returned the wrong response: {response:?}"
        );
    }

    #[test]
    fn test_production_explicit_reasoning_efforts_reach_the_request_exactly() {
        for effort in [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Xhigh,
            ReasoningEffort::Max,
        ] {
            let provider = ChatGptSubscriptionProvider::production(
                &production_credential(),
                Some(DEFAULT_MODEL),
                Some(effort),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "explicit reasoning effort {} was rejected: {error:#}",
                    effort.as_str()
                )
            });
            let request =
                ProviderRequest::new(vec![Message::user("hello")]).with_model(DEFAULT_MODEL);
            let body = responses_lite_request(&request, provider.reasoning_effort)
                .expect("validated explicit reasoning must serialize");

            assert_eq!(
                provider.requested_reasoning_effort(&request),
                Some(effort),
                "explicit effort {} changed before dispatch",
                effort.as_str()
            );
            assert_eq!(
                body["reasoning"]["effort"],
                effort.as_str(),
                "explicit effort {} changed at the request boundary: {body}",
                effort.as_str()
            );
        }
    }

    #[test]
    fn test_production_unsupported_reasoning_names_value_and_allowed_efforts() {
        for effort in [ReasoningEffort::None, ReasoningEffort::Minimal] {
            let error = ChatGptSubscriptionProvider::production(
                &production_credential(),
                Some(DEFAULT_MODEL),
                Some(effort),
            )
            .expect_err("unproven subscription reasoning effort was accepted");
            let diagnostic = error.to_string();
            assert!(
                diagnostic.contains(effort.as_str())
                    && diagnostic.contains("low, medium, high, xhigh, max"),
                "unsupported effort {} returned an unactionable diagnostic: {error:#}",
                effort.as_str()
            );
        }
    }

    struct StaticSource {
        generation: Mutex<String>,
        leases: AtomicUsize,
        refreshes: AtomicUsize,
    }

    impl StaticSource {
        fn new() -> Self {
            Self {
                generation: Mutex::new("generation-1".to_string()),
                leases: AtomicUsize::new(0),
                refreshes: AtomicUsize::new(0),
            }
        }

        async fn current(&self) -> ChatGptCredentialLease {
            ChatGptCredentialLease {
                access_token: "subscription-secret".to_string(),
                account: "account-1".to_string(),
                generation: self.generation.lock().await.clone(),
            }
        }
    }

    #[async_trait]
    impl ChatGptCredentialSource for StaticSource {
        async fn lease(&self, _cancel: &CancellationToken) -> Result<ChatGptCredentialLease> {
            self.leases.fetch_add(1, Ordering::SeqCst);
            Ok(self.current().await)
        }

        async fn refresh_after_unauthorized(
            &self,
            rejected_generation: &str,
            _cancel: &CancellationToken,
        ) -> Result<ChatGptCredentialLease> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            let mut generation = self.generation.lock().await;
            if generation.as_str() == rejected_generation {
                *generation = "generation-2".to_string();
            }
            Ok(ChatGptCredentialLease {
                access_token: "refreshed-subscription-secret".to_string(),
                account: "account-1".to_string(),
                generation: generation.clone(),
            })
        }
    }

    fn catalog_body() -> String {
        catalog_body_with_context_windows(1_050_000, 1_050_000)
    }

    fn catalog_body_with_context_windows(sol: u64, alias: u64) -> String {
        json!({
            "models":[
                {
                    "slug":"gpt-5.6-sol",
                    "supported_in_api":true,
                    "use_responses_lite":true,
                    "input_modalities":["text","image"],
                    "context_window":sol
                },
                {
                    "slug":"gpt-5.6",
                    "supported_in_api":true,
                    "use_responses_lite":true,
                    "input_modalities":["text","image"],
                    "context_window":alias
                }
            ]
        })
        .to_string()
    }

    fn single_model_catalog_body(slug: &str, context_window: u64) -> String {
        json!({
            "models":[{
                "slug":slug,
                "supported_in_api":true,
                "use_responses_lite":true,
                "input_modalities":["text","image"],
                "context_window":context_window
            }]
        })
        .to_string()
    }

    fn completed_sse_with_model_provenance(model: Option<&str>) -> String {
        let created = match model {
            Some(model) => {
                json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":model}}})
            }
            None => json!({"type":"response.created","sequence_number":1,"response":{}}),
        };
        format!(
            concat!(
                "event: response.created\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.output_text.delta\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.completed\ndata: {}\n\n",
                "data: [DONE]\n\n"
            ),
            created,
            json!({"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"type":"reasoning","summary":[],"encrypted_content":"opaque-1"}}),
            json!({"type":"response.output_text.delta","sequence_number":3,"item_id":"message-1","output_index":1,"content_index":0,"delta":"hello"}),
            json!({"type":"response.output_item.done","sequence_number":4,"output_index":1,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}),
            json!({"type":"response.output_item.done","sequence_number":5,"output_index":2,"item":{"type":"function_call","call_id":"call-2","name":"read","namespace":"functions","arguments":"{\"path\":\"README.md\"}"}}),
            json!({"type":"response.completed","sequence_number":6,"response":{"id":"resp-1","usage":{"input_tokens":12,"output_tokens":7}}})
        )
    }

    fn completed_sse(model: &str) -> String {
        completed_sse_with_model_provenance(Some(model))
    }

    fn completed_sse_with_audited_passive_fields(model: &str) -> String {
        let mut response = json!({
            "id":"resp-passive-fields",
            "object":"response",
            "created_at":1_777_777_776.5,
            "completed_at":1_777_777_777.5,
            "status":"completed",
            "error":null,
            "incomplete_details":null,
            "instructions":null,
            "max_output_tokens":null,
            "max_tool_calls":null,
            "model":model,
            "parallel_tool_calls":false,
            "previous_response_id":null,
            "reasoning":{},
            "store":false,
            "temperature":1.0,
            "text":{},
            "tool_choice":"auto",
            "tools":[],
            "top_p":1.0,
            "truncation":"disabled"
        })
        .as_object()
        .expect("passive-field response fixture must be an object")
        .clone();
        response.extend(
            json!({
                "usage":{"input_tokens":12,"output_tokens":7,"total_tokens":19},
                "user":null,
                "metadata":{"fixture":"audited-passive-fields"},
                "service_tier":"default",
                "prompt_cache_key":null,
                "prompt_cache_diagnostics":{"type":"cache_hit"},
                "prompt_cache_options":{"mode":"implicit","ttl":"30m","comparison_response_id":"resp-cache-comparison"},
                "prompt_cache_retention":"24h",
                "safety_identifier":null,
                "headers":{},
                "usage_metadata":{},
                "end_turn":true,
                "background":null,
                "conversation":{"id":"conv-fixture"},
                "moderation":{},
                "prompt":{},
                "top_logprobs":0,
                "frequency_penalty":0.0,
                "presence_penalty":0.0,
                "tool_usage":[{"name":"read","calls":1}]
            })
            .as_object()
            .expect("passive-field response extension fixture must be an object")
                .clone(),
        );
        response.insert(
            "output".to_string(),
            json!([
                {"type":"reasoning","summary":[],"encrypted_content":"opaque-1"},
                {"id":"message-terminal","type":"message","status":"completed","role":"assistant","phase":"final_answer","internal_chat_message_metadata_passthrough":{"source":"terminal"},"content":[{"type":"output_text","text":"hello"}]}
            ]),
        );
        let completed = json!({
            "type":"response.completed",
            "sequence_number":5,
            "obfuscation":"padding-terminal",
            "safety_buffering":{"enabled":false},
            "response":response
        });
        format!(
            concat!(
                "event: response.created\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.output_text.delta\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.completed\ndata: {}\n\n",
                "data: [DONE]\n\n"
            ),
            json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":model}},"obfuscation":"padding-created","safety_buffering":{"enabled":false}}),
            json!({"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"type":"reasoning","summary":[],"encrypted_content":"opaque-1"},"obfuscation":"padding-reasoning"}),
            json!({"type":"response.output_text.delta","sequence_number":3,"item_id":"message-1","output_index":1,"content_index":0,"delta":"hello","logprobs":[],"obfuscation":"padding-delta"}),
            json!({"type":"response.output_item.done","sequence_number":4,"output_index":1,"item":{"id":"message-stream","type":"message","status":"completed","role":"assistant","phase":"final_answer","internal_chat_message_metadata_passthrough":{"source":"stream"},"content":[{"type":"output_text","text":"hello","annotations":[],"logprobs":[]}]},"obfuscation":"padding-message"}),
            completed
        )
    }

    fn completed_sse_with_terminal_output(model: &str, terminal_output: Value) -> String {
        format!(
            concat!(
                "event: response.created\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.output_text.delta\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.completed\ndata: {}\n\n",
                "data: [DONE]\n\n"
            ),
            json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":model}}}),
            json!({"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"type":"reasoning","summary":[],"encrypted_content":"opaque-1"}}),
            json!({"type":"response.output_text.delta","sequence_number":3,"item_id":"message-1","output_index":1,"content_index":0,"delta":"hello"}),
            json!({"type":"response.output_item.done","sequence_number":4,"output_index":1,"item":{"id":"message-stream","type":"message","status":"completed","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"hello"}]}}),
            json!({"type":"response.completed","sequence_number":5,"response":{"id":"resp-terminal-subset","status":"completed","model":model,"output":terminal_output,"usage":{"input_tokens":12,"output_tokens":7}}})
        )
    }

    fn completed_sse_with_streamed_message_and_terminal_response(
        model: &str,
        terminal_response: Value,
    ) -> String {
        format!(
            concat!(
                "event: response.created\ndata: {}\n\n",
                "event: response.output_text.delta\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.completed\ndata: {}\n\n",
                "data: [DONE]\n\n"
            ),
            json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":model}}}),
            json!({"type":"response.output_text.delta","sequence_number":2,"item_id":"message-1","output_index":0,"content_index":0,"delta":"hello"}),
            json!({"type":"response.output_item.done","sequence_number":3,"output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}),
            json!({"type":"response.completed","sequence_number":4,"response":terminal_response})
        )
    }

    fn completed_sse_with_streamed_message_and_empty_terminal_output(model: &str) -> String {
        completed_sse_with_streamed_message_and_terminal_response(
            model,
            json!({
                "id":"resp-empty-terminal-output",
                "status":"completed",
                "model":model,
                "output":[],
                "usage":{"input_tokens":12,"output_tokens":7}
            }),
        )
    }

    async fn run_streamed_message_terminal_response(
        terminal_response: Value,
    ) -> (
        Result<ProviderResponse, String>,
        Vec<Result<StreamChunk, String>>,
    ) {
        run_streamed_message_sse(completed_sse_with_streamed_message_and_terminal_response(
            DEFAULT_MODEL,
            terminal_response,
        ))
        .await
    }

    async fn run_streamed_message_sse(
        response_body: String,
    ) -> (
        Result<ProviderResponse, String>,
        Vec<Result<StreamChunk, String>>,
    ) {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_header("x-codex-primary-used-percent", "25.5")
            .with_body(response_body)
            .expect(2)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .expect("terminal-response fixture must construct a provider");
        let request = ProviderRequest::new(vec![Message::user("hello")]);

        let (buffered, streaming) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );

        models.assert_async().await;
        inference.assert_async().await;
        let buffered = buffered.map_err(|error| error.to_string());
        let mut receiver = streaming.expect("stream setup must consume the complete SSE fixture");
        let mut outcome = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
        (buffered, outcome)
    }

    fn completed_sse_with_streamed_tool_and_terminal_output(
        model: &str,
        terminal_output: Value,
    ) -> String {
        format!(
            concat!(
                "event: response.created\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.output_text.delta\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.completed\ndata: {}\n\n",
                "data: [DONE]\n\n"
            ),
            json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":model}}}),
            json!({"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"type":"reasoning","summary":[],"encrypted_content":"opaque-1"}}),
            json!({"type":"response.output_text.delta","sequence_number":3,"item_id":"message-1","output_index":1,"content_index":0,"delta":"hello"}),
            json!({"type":"response.output_item.done","sequence_number":4,"output_index":1,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}),
            json!({"type":"response.output_item.done","sequence_number":5,"output_index":2,"item":{"type":"function_call","call_id":"call-2","name":"read","namespace":"functions","arguments":"{\"path\":\"README.md\"}"}}),
            json!({"type":"response.completed","sequence_number":6,"response":{"id":"resp-terminal-negative","status":"completed","model":model,"output":terminal_output,"usage":{"input_tokens":12,"output_tokens":7}}})
        )
    }

    async fn assert_terminal_snapshot_rejected_at_provider_boundary(
        case: &str,
        terminal_output: Value,
        expected_diagnostic: &str,
    ) {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_header("x-codex-primary-used-percent", "25.5")
            .with_body(completed_sse_with_streamed_tool_and_terminal_output(
                DEFAULT_MODEL,
                terminal_output,
            ))
            .expect(2)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap_or_else(|error| panic!("{case} fixture failed to construct a provider: {error:#}"));
        let request = ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]);

        let (buffered, streaming) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );

        models.assert_async().await;
        inference.assert_async().await;
        let buffered_error = buffered.err().unwrap_or_else(|| {
            panic!("{case} terminal semantic drift passed the buffered boundary")
        });
        assert!(
            buffered_error.to_string().contains(expected_diagnostic),
            "{case} buffered rejection was not actionable: {buffered_error:#}"
        );

        let mut receiver = streaming.unwrap_or_else(|error| {
            panic!("{case} stream setup failed before the SSE fixture was consumed: {error:#}")
        });
        let mut outcome = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
        assert_eq!(
            outcome.iter().filter(|chunk| chunk.is_err()).count(),
            1,
            "{case} must emit exactly one terminal stream error; outcome={outcome:?}"
        );
        assert!(
            matches!(outcome.last(), Some(Err(error)) if error.contains(expected_diagnostic)),
            "{case} did not end with its actionable stream error; outcome={outcome:?}"
        );
        assert!(
            outcome.iter().all(|chunk| !matches!(
                chunk,
                Ok(StreamChunk::ResponseMetadata { .. }
                    | StreamChunk::Usage { .. }
                    | StreamChunk::Allowance { .. }
                    | StreamChunk::ContentBlockComplete(_))
            )),
            "{case} published terminal metadata, usage, allowance, or completed content after \
             semantic reconciliation failed; outcome={outcome:?}"
        );
    }

    fn tool() -> ToolDefinition {
        ToolDefinition {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            input_schema: ToolInputSchema {
                schema_type: "object".to_string(),
                properties: json!({"path":{"type":"string"}}),
                required: vec!["path".to_string()],
            },
        }
    }

    async fn stalling_subscription_server(
        send_stream_headers: bool,
    ) -> (String, tokio::sync::oneshot::Receiver<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut catalog_socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 16 * 1024];
            let _ = catalog_socket.read(&mut request).await;
            let catalog = catalog_body();
            catalog_socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        catalog.len(),
                        catalog
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            catalog_socket.flush().await.unwrap();
            drop(catalog_socket);

            let (mut response_socket, _) = listener.accept().await.unwrap();
            let _ = response_socket.read(&mut request).await;
            if send_stream_headers {
                response_socket
                    .write_all(
                        concat!(
                            "HTTP/1.1 200 OK\r\n",
                            "content-type: text/event-stream\r\n",
                            "openai-model: gpt-5.6-sol\r\n",
                            "connection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                response_socket.flush().await.unwrap();
            }
            let mut byte = [0u8; 1];
            while matches!(response_socket.read(&mut byte).await, Ok(1)) {}
            let _ = closed_tx.send(());
        });
        (format!("http://{address}/backend-api/codex"), closed_rx)
    }

    async fn fragmented_subscription_server(body: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut request = vec![0u8; 16 * 1024];
            let (mut catalog_socket, _) = listener.accept().await.unwrap();
            let _ = catalog_socket.read(&mut request).await;
            let catalog = catalog_body();
            catalog_socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        catalog.len(),
                        catalog
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            catalog_socket.flush().await.unwrap();
            drop(catalog_socket);

            let (mut response_socket, _) = listener.accept().await.unwrap();
            let _ = response_socket.read(&mut request).await;
            response_socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nopenai-model: {DEFAULT_MODEL}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            for byte in body.bytes() {
                response_socket.write_all(&[byte]).await.unwrap();
                response_socket.flush().await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        format!("http://{address}/backend-api/codex")
    }

    async fn subscription_stream_outcome(
        body: String,
        header_model: &str,
    ) -> Vec<std::result::Result<StreamChunk, String>> {
        let mut server = mockito::Server::new_async().await;
        let _models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .create_async()
            .await;
        let _inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", header_model)
            .with_body(body)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap();
        let mut receiver = provider
            .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .unwrap();
        let mut outcome = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
        outcome
    }

    #[test]
    fn canonical_request_preserves_ordered_reasoning_tools_results_and_lite_shape() {
        let request = ProviderRequest::new(vec![
            Message::with_content(
                "user",
                vec![
                    ContentBlock::text("inspect"),
                    ContentBlock::image("image/png", VALID_PNG_BASE64),
                ],
            ),
            Message::with_content(
                "assistant",
                vec![
                    ContentBlock::opaque_reasoning("encrypted-turn"),
                    ContentBlock::ToolUse {
                        id: "call-1".to_string(),
                        name: "read".to_string(),
                        input: json!({"path":"README.md"}),
                    },
                ],
            ),
            Message::with_content(
                "user",
                vec![ContentBlock::ToolResult {
                    tool_use_id: "call-1".to_string(),
                    content: "contents".to_string(),
                    is_error: None,
                }],
            ),
        ])
        .with_model(DEFAULT_MODEL)
        .with_system("developer instructions")
        .with_tools(vec![tool()]);
        let body = responses_lite_request(&request, ReasoningEffort::High).unwrap();
        assert_eq!(body["input"][0]["type"], "additional_tools");
        assert_eq!(body["input"][0]["role"], "developer");
        assert_eq!(
            body["input"][0]["id"],
            "at_06f0d744-9e74-54f8-9371-312adc3c666b"
        );
        assert_eq!(body["input"][0]["tools"][0]["type"], "namespace");
        assert_eq!(body["input"][0]["tools"][0]["name"], "functions");
        assert_eq!(body["input"][0]["tools"][0]["description"], "");
        assert_eq!(body["input"][0]["tools"][0]["tools"][0]["type"], "function");
        assert_eq!(body["input"][1]["role"], "developer");
        assert_eq!(
            body["input"][1]["id"],
            "msg_e26db3f8-834d-58b1-9d5e-5e9465345d82"
        );
        assert_eq!(body["input"][2]["content"][1]["type"], "input_image");
        assert_eq!(
            body["input"][2]["content"][1]["image_url"],
            format!("data:image/png;base64,{VALID_PNG_BASE64}")
        );
        assert_eq!(body["input"][3]["type"], "reasoning");
        assert_eq!(body["input"][4]["type"], "function_call");
        assert_eq!(body["input"][4]["namespace"], "functions");
        assert_eq!(body["input"][5]["type"], "function_call_output");
        assert_eq!(body["reasoning"]["context"], "all_turns");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert!(body.get("instructions").is_none());
        assert!(body.get("tools").is_none());
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("prompt_cache_key").is_none());
        assert!(body.get("client_metadata").is_none());
    }

    #[test]
    fn responses_lite_prompt_item_ids_are_stable_and_payload_bound() {
        let request = ProviderRequest::new(vec![Message::user("hello")])
            .with_model(DEFAULT_MODEL)
            .with_system("developer instructions")
            .with_tools(vec![tool()]);
        let first = responses_lite_request(&request, ReasoningEffort::High).unwrap();
        let retry = responses_lite_request(&request, ReasoningEffort::High).unwrap();
        assert_eq!(first["input"][0]["id"], retry["input"][0]["id"]);
        assert_eq!(first["input"][1]["id"], retry["input"][1]["id"]);

        let changed = responses_lite_request(
            &request.with_system("different developer instructions"),
            ReasoningEffort::High,
        )
        .unwrap();
        assert_eq!(first["input"][0]["id"], changed["input"][0]["id"]);
        assert_ne!(first["input"][1]["id"], changed["input"][1]["id"]);
        assert!(first["input"][0]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("at_") && id.len() == 39));
        assert!(first["input"][1]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("msg_") && id.len() == 40));
    }

    #[test]
    fn catalog_and_capability_alias_are_pinned_to_responses_lite() {
        let catalog = parse_catalog(catalog_body().as_bytes()).unwrap();
        assert!(catalog.models.contains_key(DEFAULT_MODEL));
        assert!(catalog.models.contains_key(MODEL_ALIAS));
        for model in [DEFAULT_MODEL, MODEL_ALIAS] {
            let capability = subscription_capabilities(model);
            assert_eq!(
                capability.wire_protocol.protocol,
                Some(WireProtocol::OpenAiChatGptResponsesLite)
            );
            assert!(capability.image_input.is_supported());
            assert!(capability.continuation.is_supported());
            assert_eq!(capability.context_window.max_tokens, None);
        }
        assert!(subscription_capabilities("gpt-4o")
            .wire_protocol
            .protocol
            .is_none());
    }

    #[test]
    fn catalog_accepts_and_preserves_authoritative_bounded_context_windows() {
        for (sol, alias) in [(200_000, 262_144), (1_000_000, 1_050_000)] {
            let catalog =
                parse_catalog(catalog_body_with_context_windows(sol, alias).as_bytes()).unwrap();
            assert_eq!(catalog.models[DEFAULT_MODEL].context_window, sol as usize);
            assert_eq!(catalog.models[MODEL_ALIAS].context_window, alias as usize);
        }
        for slug in [DEFAULT_MODEL, MODEL_ALIAS] {
            let catalog = parse_catalog(single_model_catalog_body(slug, 1_000_000).as_bytes())
                .expect("either exact selectable identifier is sufficient");
            assert_eq!(catalog.models.len(), 1);
            assert_eq!(catalog.models[slug].context_window, 1_000_000);
        }
    }

    #[test]
    fn catalog_rejects_missing_malformed_zero_and_excessive_context_windows() {
        let assert_invalid = |catalog: Value| {
            let error = parse_catalog(catalog.to_string().as_bytes())
                .err()
                .expect("invalid context window must fail");
            assert!(error.is::<SubscriptionCatalogContextWindowInvalid>());
            error.to_string()
        };

        let mut missing: Value = serde_json::from_str(&catalog_body()).unwrap();
        missing["models"][0]
            .as_object_mut()
            .unwrap()
            .remove("context_window");
        assert!(assert_invalid(missing).contains("missing or malformed"));

        let mut malformed: Value = serde_json::from_str(&catalog_body()).unwrap();
        malformed["models"][0]["context_window"] = json!("1000000");
        assert!(assert_invalid(malformed).contains("missing or malformed"));

        let mut zero: Value = serde_json::from_str(&catalog_body()).unwrap();
        zero["models"][0]["context_window"] = json!(0);
        let zero_error = assert_invalid(zero);
        assert!(zero_error.contains("context window 0"));
        assert!(!zero_error.contains("models"));

        let mut excessive: Value = serde_json::from_str(&catalog_body()).unwrap();
        excessive["models"][0]["context_window"] = json!(MAX_CATALOG_CONTEXT_WINDOW + 1);
        let excessive_error = assert_invalid(excessive);
        assert!(excessive_error.contains(&(MAX_CATALOG_CONTEXT_WINDOW + 1).to_string()));
        assert!(!excessive_error.contains("models"));
    }

    #[test]
    fn catalog_context_metadata_does_not_weaken_slug_api_or_modality_checks() {
        let mut wrong_slug: Value =
            serde_json::from_str(&single_model_catalog_body(DEFAULT_MODEL, 1_000_000)).unwrap();
        wrong_slug["models"][0]["slug"] = json!("gpt-5.6-sol-impostor");
        let wrong_slug_error = parse_catalog(wrong_slug.to_string().as_bytes())
            .err()
            .expect("wrong slug must fail");
        assert!(wrong_slug_error.is::<SubscriptionCatalogNoSelectableModel>());

        let mut unsupported_api: Value =
            serde_json::from_str(&single_model_catalog_body(DEFAULT_MODEL, 1_000_000)).unwrap();
        unsupported_api["models"][0]["supported_in_api"] = json!(false);
        let unsupported_error = parse_catalog(unsupported_api.to_string().as_bytes())
            .err()
            .expect("unsupported API model must fail");
        assert!(unsupported_error.is::<SubscriptionCatalogNoSelectableModel>());

        let mut missing_image: Value =
            serde_json::from_str(&single_model_catalog_body(DEFAULT_MODEL, 1_000_000)).unwrap();
        missing_image["models"][0]["input_modalities"] = json!(["text"]);
        assert!(parse_catalog(missing_image.to_string().as_bytes())
            .err()
            .expect("missing image modality must fail")
            .to_string()
            .contains("catalog capabilities drifted"));

        let mut missing_text: Value =
            serde_json::from_str(&single_model_catalog_body(DEFAULT_MODEL, 1_000_000)).unwrap();
        missing_text["models"][0]["input_modalities"] = json!(["image"]);
        let missing_text_error = parse_catalog(missing_text.to_string().as_bytes())
            .err()
            .expect("missing text modality must fail");
        assert!(missing_text_error.is::<SubscriptionCatalogNoSelectableModel>());
    }

    #[test]
    fn test_compatibility_version_is_pinned_to_audited_codex_not_finch_package() {
        assert_eq!(CHATGPT_CATALOG_CLIENT_VERSION, "0.151.0");
        assert_ne!(CHATGPT_CATALOG_CLIENT_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn chatgpt_user_agent_is_static_bounded_and_has_no_user_identity() {
        assert_eq!(
            FINCH_CHATGPT_USER_AGENT,
            concat!(
                "finch/",
                env!("CARGO_PKG_VERSION"),
                " (+https://darwin-finch.github.io/)"
            )
        );
        assert!(FINCH_CHATGPT_USER_AGENT.len() <= 256);
        for private_value in [
            "shammah",
            "Shammahs-MacBook-Air.local",
            "brain-identifier",
            "account-identifier",
            "credential-identifier",
        ] {
            assert!(!FINCH_CHATGPT_USER_AGENT.contains(private_value));
        }
    }

    #[tokio::test]
    async fn empty_catalog_is_typed_actionable_and_secret_free() {
        let access_secret = "empty-catalog-access-secret";
        let account_secret = "empty-catalog-account-secret";
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                "0.151.0".into(),
            ))
            .match_header("authorization", format!("Bearer {access_secret}").as_str())
            .match_header("chatgpt-account-id", account_secret)
            .match_header("originator", "finch")
            .match_header("user-agent", FINCH_CHATGPT_USER_AGENT)
            .with_status(200)
            .with_body(json!({"models": []}).to_string())
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap();
        let error = provider
            .account_catalog(
                &ChatGptCredentialLease {
                    access_token: access_secret.into(),
                    account: account_secret.into(),
                    generation: "generation-empty-catalog".into(),
                },
                &CancellationToken::new(),
            )
            .await
            .err()
            .expect("empty catalog must fail");
        assert!(error.is::<SubscriptionCatalogUnavailable>());
        let rendered = error.to_string();
        assert!(rendered.contains("pinned Codex compatibility version 0.151.0"));
        assert!(rendered.contains("entitlement or server compatibility filtering"));
        assert!(!rendered.contains(access_secret));
        assert!(!rendered.contains(account_secret));
        models.assert_async().await;
    }

    #[test]
    fn catalog_ignores_unrelated_models_and_accepts_one_pinned_identifier() {
        let mut catalog: Value = serde_json::from_str(&catalog_body()).unwrap();
        catalog["models"].as_array_mut().unwrap().push(json!({
            "slug":"unrelated-account-model",
            "context_window":"attacker-controlled-not-a-number"
        }));
        let parsed = parse_catalog(catalog.to_string().as_bytes()).unwrap();
        assert_eq!(parsed.models.len(), 2);

        catalog["models"]
            .as_array_mut()
            .unwrap()
            .retain(|model| model["slug"] != MODEL_ALIAS);
        let parsed = parse_catalog(catalog.to_string().as_bytes())
            .expect("one exact selectable identifier is sufficient");
        assert_eq!(parsed.models.len(), 1);
        assert!(parsed.models.contains_key(DEFAULT_MODEL));
    }

    #[test]
    fn test_actual_model_uses_authoritative_header_or_requested_route_and_never_payload_model() {
        let allowed = HashSet::new();
        let terminal = json!({
            "id":"resp-1",
            "status":"completed",
            "model":"attacker-payload-model",
            "output":[],
            "usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}
        });
        let mut accumulator = StreamAccumulator::default();
        let completed = parse_completed(
            terminal.as_object().unwrap(),
            DEFAULT_MODEL,
            Some(DEFAULT_MODEL),
            &allowed,
            &mut accumulator,
        )
        .expect("a valid authoritative header must complete successfully");
        assert_eq!(
            completed.model, DEFAULT_MODEL,
            "authoritative header model was not retained; observed_model={}",
            completed.model
        );

        let mut accumulator = StreamAccumulator::default();
        let completed = parse_completed(
            terminal.as_object().unwrap(),
            DEFAULT_MODEL,
            None,
            &allowed,
            &mut accumulator,
        )
        .expect("missing authoritative model provenance must retain the validated requested route");
        assert_eq!(
            completed.model, DEFAULT_MODEL,
            "missing provenance must not copy the untrusted terminal payload model"
        );
    }

    #[test]
    fn test_terminal_background_accepts_null_and_false_but_rejects_other_values() {
        for (case, value) in [("null", Value::Null), ("false", json!(false))] {
            let terminal = json!({
                "id":"resp-background",
                "status":"completed",
                "model":DEFAULT_MODEL,
                "output":[],
                "background":value
            });
            parse_completed(
                terminal.as_object().unwrap(),
                DEFAULT_MODEL,
                Some(DEFAULT_MODEL),
                &HashSet::new(),
                &mut StreamAccumulator::default(),
            )
            .unwrap_or_else(|error| panic!("documented {case} background state failed: {error:#}"));
        }

        for (case, value) in [("true", json!(true)), ("string", json!("false"))] {
            let background = json!({
                "id":"resp-background",
                "status":"completed",
                "model":DEFAULT_MODEL,
                "output":[],
                "background":value
            });
            let error = parse_completed(
                background.as_object().unwrap(),
                DEFAULT_MODEL,
                Some(DEFAULT_MODEL),
                &HashSet::new(),
                &mut StreamAccumulator::default(),
            )
            .err()
            .unwrap_or_else(|| panic!("{case} background execution unexpectedly succeeded"));
            assert!(
                error.to_string().contains("background state was invalid"),
                "{case} background state returned an unhelpful diagnostic: {error:#}"
            );
        }
    }

    #[test]
    fn test_terminal_tool_usage_is_bounded_without_requiring_an_object() {
        let terminal = json!({
            "id":"resp-tool-usage",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            "tool_usage":[{"name":"read","calls":1}]
        });
        parse_completed(
            terminal.as_object().unwrap(),
            DEFAULT_MODEL,
            Some(DEFAULT_MODEL),
            &HashSet::new(),
            &mut StreamAccumulator::default(),
        )
        .expect("bounded passive tool usage must not require a provider-specific shape");

        for value in [json!(true), json!("x".repeat(MAX_TOOL_ARGUMENT_BYTES - 2))] {
            let mut bounded = terminal.clone();
            bounded["tool_usage"] = value;
            parse_completed(
                bounded.as_object().unwrap(),
                DEFAULT_MODEL,
                Some(DEFAULT_MODEL),
                &HashSet::new(),
                &mut StreamAccumulator::default(),
            )
            .expect("opaque tool usage at or below the inclusive byte limit must be accepted");
        }

        let mut excessive = terminal;
        excessive["tool_usage"] = json!("x".repeat(MAX_TOOL_ARGUMENT_BYTES - 1));
        let error = parse_completed(
            excessive.as_object().unwrap(),
            DEFAULT_MODEL,
            Some(DEFAULT_MODEL),
            &HashSet::new(),
            &mut StreamAccumulator::default(),
        )
        .err()
        .expect("oversized passive tool usage must remain bounded");
        assert!(
            error
                .to_string()
                .contains("tool usage metadata exceeded the size limit"),
            "oversized tool usage returned an unhelpful diagnostic: {error:#}"
        );
    }

    #[test]
    fn test_terminal_unknown_fields_remain_fail_closed() {
        let sentinel = "sk-proj-SensitiveToken123";
        let terminal = json!({
            "id":"resp-unknown-terminal",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            sentinel:true
        });
        let error = parse_completed(
            terminal.as_object().unwrap(),
            DEFAULT_MODEL,
            Some(DEFAULT_MODEL),
            &HashSet::new(),
            &mut StreamAccumulator::default(),
        )
        .err()
        .expect("unaudited terminal semantics must remain fail closed");
        assert_eq!(
            error.to_string(),
            "ChatGPT subscription terminal response contained an unknown field",
            "unknown terminal semantics must report only the static containing object"
        );
        assert!(
            !error.to_string().contains(sentinel),
            "unknown terminal semantics reflected response-body field data"
        );
    }

    #[test]
    fn test_usage_extra_requires_object_metadata() {
        for value in [json!(false), json!("opaque")] {
            let usage = json!({
                "input_tokens":12,
                "output_tokens":7,
                "total_tokens":19,
                "extra":value
            });
            let error = parse_usage(Some(&usage))
                .err()
                .expect("non-object usage extra metadata must remain fail closed");
            assert_eq!(
                error.to_string(),
                "ChatGPT response usage extra metadata was invalid",
                "non-object usage extra metadata returned an unhelpful diagnostic"
            );
        }
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_reject_non_object_usage_extra_before_terminal_effects() {
        let terminal = json!({
            "id":"resp-invalid-usage-extra",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            "usage":{
                "input_tokens":12,
                "output_tokens":7,
                "total_tokens":19,
                "extra":false
            }
        });
        let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
        let expected = "ChatGPT response usage extra metadata was invalid";
        assert_eq!(
            buffered.err().as_deref(),
            Some(expected),
            "non-object usage extra crossed the buffered provider boundary"
        );
        assert!(
            matches!(
                outcome.as_slice(),
                [Ok(StreamChunk::TextDelta(delta)), Err(error)]
                    if delta == "hello" && error == expected
            ),
            "non-object usage extra must end with one error and no terminal effects; \
             outcome={outcome:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_bound_multiline_usage_extra_event() {
        let first_items = "0,".repeat(300_000);
        let second_items = "0,".repeat(300_000);
        let response_body = format!(
            concat!(
                "event: response.created\ndata: {}\n\n",
                "event: response.output_text.delta\ndata: {}\n\n",
                "event: response.output_item.done\ndata: {}\n\n",
                "event: response.completed\n",
                "data: {{\"type\":\"response.completed\",\"sequence_number\":4,",
                "\"response\":{{\"id\":\"resp-oversized-usage-extra\",",
                "\"status\":\"completed\",\"model\":\"{}\",\"output\":[],",
                "\"usage\":{{\"input_tokens\":12,\"output_tokens\":7,",
                "\"total_tokens\":19,\"extra\":{{\"items\":[{}\n",
                "data: {}0]}}}}}}}}\n\n"
            ),
            json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":DEFAULT_MODEL}}}),
            json!({"type":"response.output_text.delta","sequence_number":2,"item_id":"message-1","output_index":0,"content_index":0,"delta":"hello"}),
            json!({"type":"response.output_item.done","sequence_number":3,"output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}),
            DEFAULT_MODEL,
            first_items,
            second_items,
        );
        let completed_event = response_body
            .split("event: response.completed\n")
            .nth(1)
            .expect("oversized completed event fixture must exist");
        assert!(
            completed_event.len() > MAX_SSE_EVENT_BYTES,
            "oversized completed event fixture drifted below the aggregate limit"
        );
        assert!(
            completed_event
                .lines()
                .all(|line| line.len() <= MAX_SSE_LINE_BYTES),
            "aggregate event fixture accidentally exceeded the per-line limit"
        );

        let (buffered, outcome) = run_streamed_message_sse(response_body).await;
        let expected = "ChatGPT subscription stream event exceeded the size limit";
        assert_eq!(
            buffered.err().as_deref(),
            Some(expected),
            "oversized multiline usage event crossed the buffered provider boundary"
        );
        assert!(
            matches!(
                outcome.as_slice(),
                [Ok(StreamChunk::TextDelta(delta)), Err(error)]
                    if delta == "hello" && error == expected
            ),
            "oversized multiline usage event must end with one error and no terminal effects; \
             outcome={outcome:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_accept_large_usage_extra_within_event_bound() {
        let large_extra = json!({"p":"x".repeat(128 * 1024 - 8)});
        assert_eq!(
            serde_json::to_vec(&large_extra).unwrap().len(),
            128 * 1024,
            "large usage-extra fixture drifted"
        );
        let terminal = json!({
            "id":"resp-large-usage-extra",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            "usage":{
                "input_tokens":12,
                "output_tokens":7,
                "total_tokens":19,
                "extra":large_extra
            }
        });
        let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
        let buffered = buffered.unwrap_or_else(|error| {
            panic!("large usage extra metadata within the event bound failed: {error}")
        });
        assert_eq!(
            buffered
                .usage
                .as_ref()
                .map(|usage| (usage.input_tokens, usage.output_tokens)),
            Some((12, 7)),
            "large passive metadata changed buffered token accounting"
        );
        assert!(
            matches!(
                outcome.as_slice(),
                [
                    Ok(StreamChunk::TextDelta(delta)),
                    Ok(StreamChunk::ResponseMetadata { model }),
                    Ok(StreamChunk::Usage {
                        input_tokens: 12,
                        output_tokens: 7,
                    }),
                    Ok(StreamChunk::Allowance {
                        primary_used_percent: Some(primary),
                        secondary_used_percent: None,
                    }),
                    Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text })),
                ] if delta == "hello"
                    && model == DEFAULT_MODEL
                    && *primary == 25.5
                    && text == "hello"
            ),
            "large passive metadata changed exact ordered streaming effects; outcome={outcome:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_accept_bounded_usage_extra_metadata() {
        let terminal = json!({
            "id":"resp-usage-extra",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            "usage":{
                "input_tokens":12,
                "output_tokens":7,
                "total_tokens":19,
                "extra":{"label":"example","items":[0,null,true]}
            }
        });
        let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
        let buffered = buffered.unwrap_or_else(|error| {
            panic!("bounded official usage extra metadata failed buffered parsing: {error}")
        });
        assert!(
            matches!(buffered.content.as_slice(), [ContentBlock::Text { text }] if text == "hello"),
            "usage extra metadata changed buffered semantic content; response={buffered:?}"
        );
        assert_eq!(
            buffered
                .usage
                .as_ref()
                .map(|usage| (usage.input_tokens, usage.output_tokens)),
            Some((12, 7)),
            "usage extra metadata changed token accounting"
        );
        assert_eq!(
            buffered.model, DEFAULT_MODEL,
            "usage extra metadata changed buffered model provenance"
        );
        assert!(
            matches!(
                outcome.as_slice(),
                [
                    Ok(StreamChunk::TextDelta(delta)),
                    Ok(StreamChunk::ResponseMetadata { model }),
                    Ok(StreamChunk::Usage {
                        input_tokens: 12,
                        output_tokens: 7,
                    }),
                    Ok(StreamChunk::Allowance {
                        primary_used_percent: Some(primary),
                        secondary_used_percent: None,
                    }),
                    Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text })),
                ] if delta == "hello"
                    && model == DEFAULT_MODEL
                    && *primary == 25.5
                    && text == "hello"
            ),
            "usage extra metadata did not preserve exact ordered streaming effects; \
             outcome={outcome:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_accept_bounded_usage_attribution_metadata() {
        let terminal = json!({
            "id":"resp-usage-attribution",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            "usage":{
                "input_tokens":12,
                "output_tokens":7,
                "total_tokens":19,
                "attribution":{
                    "items":{
                        "dynamic-attribution-id":{
                            "input_tokens":12,
                            "output_tokens":7
                        }
                    }
                }
            }
        });
        let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
        let buffered = buffered.unwrap_or_else(|error| {
            panic!("bounded usage attribution metadata failed buffered parsing: {error}")
        });
        assert!(
            matches!(buffered.content.as_slice(), [ContentBlock::Text { text }] if text == "hello"),
            "usage attribution metadata changed buffered semantic content; response={buffered:?}"
        );
        assert_eq!(
            buffered
                .usage
                .as_ref()
                .map(|usage| (usage.input_tokens, usage.output_tokens)),
            Some((12, 7)),
            "usage attribution metadata changed token accounting"
        );
        assert_eq!(
            buffered.model, DEFAULT_MODEL,
            "usage attribution metadata changed buffered model provenance"
        );
        assert!(
            matches!(
                outcome.as_slice(),
                [
                    Ok(StreamChunk::TextDelta(delta)),
                    Ok(StreamChunk::ResponseMetadata { model }),
                    Ok(StreamChunk::Usage {
                        input_tokens: 12,
                        output_tokens: 7,
                    }),
                    Ok(StreamChunk::Allowance {
                        primary_used_percent: Some(primary),
                        secondary_used_percent: None,
                    }),
                    Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text })),
                ] if delta == "hello"
                    && model == DEFAULT_MODEL
                    && *primary == 25.5
                    && text == "hello"
            ),
            "usage attribution metadata did not preserve exact ordered streaming effects; \
             outcome={outcome:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_reject_invalid_usage_attribution_before_terminal_effects()
    {
        for (case, attribution, expected) in [
            (
                "non-object",
                json!(false),
                "ChatGPT response usage attribution metadata was invalid",
            ),
            (
                "oversized-object",
                json!({"payload":"x".repeat(MAX_USAGE_METADATA_BYTES)}),
                "ChatGPT response usage attribution metadata exceeded the size limit",
            ),
        ] {
            let terminal = json!({
                "id":format!("resp-invalid-usage-attribution-{case}"),
                "status":"completed",
                "model":DEFAULT_MODEL,
                "output":[],
                "usage":{
                    "input_tokens":12,
                    "output_tokens":7,
                    "total_tokens":19,
                    "attribution":attribution
                }
            });
            let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
            assert_eq!(
                buffered.err().as_deref(),
                Some(expected),
                "{case} usage attribution crossed the buffered provider boundary"
            );
            assert!(
                matches!(
                    outcome.as_slice(),
                    [Ok(StreamChunk::TextDelta(delta)), Err(error)]
                        if delta == "hello" && error == expected
                ),
                "{case} usage attribution must end with one error and no terminal effects; \
                 outcome={outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_unknown_usage_fields_report_only_static_location() {
        let sentinel = "sk-proj-SensitiveToken123";
        let mut usage = json!({
            "input_tokens":12,
            "output_tokens":7,
            "total_tokens":19
        });
        usage
            .as_object_mut()
            .expect("usage fixture must be an object")
            .insert(sentinel.to_string(), Value::Null);
        let terminal = json!({
            "id":"resp-unknown-usage",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            "usage":usage
        });
        let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
        let expected = "ChatGPT subscription response usage contained an unknown field";
        let buffered_error = buffered
            .err()
            .expect("unknown usage semantics passed the buffered provider boundary");
        assert_eq!(buffered_error, expected);
        assert!(
            !buffered_error.contains(sentinel),
            "buffered error reflected response-body field data"
        );
        assert!(
            matches!(
                outcome.as_slice(),
                [Ok(StreamChunk::TextDelta(delta)), Err(error)]
                    if delta == "hello" && error == expected && !error.contains(sentinel)
            ),
            "unknown usage semantics must end with exactly one static-location error and no \
             terminal metadata, usage, allowance, or completed content; outcome={outcome:?}"
        );
    }

    #[test]
    fn test_terminal_prompt_cache_diagnostics_are_object_shaped_and_bounded() {
        let exactly_bounded = json!({"p":"x".repeat(MAX_TOOL_ARGUMENT_BYTES - 8)});
        assert_eq!(
            serde_json::to_vec(&exactly_bounded).unwrap().len(),
            MAX_TOOL_ARGUMENT_BYTES,
            "prompt cache diagnostics exact-bound fixture drifted"
        );
        validate_documented_response_metadata(
            json!({"prompt_cache_diagnostics":exactly_bounded})
                .as_object()
                .unwrap(),
        )
        .expect("prompt cache diagnostics at the inclusive byte limit must be accepted");

        for (case, value) in [
            ("non-object", json!(false)),
            (
                "oversized-object",
                json!({"payload":"x".repeat(MAX_TOOL_ARGUMENT_BYTES)}),
            ),
        ] {
            let terminal = json!({
                "id":"resp-cache-diagnostics",
                "status":"completed",
                "model":DEFAULT_MODEL,
                "output":[],
                "prompt_cache_diagnostics":value
            });
            let error = parse_completed(
                terminal.as_object().unwrap(),
                DEFAULT_MODEL,
                Some(DEFAULT_MODEL),
                &HashSet::new(),
                &mut StreamAccumulator::default(),
            )
            .err()
            .unwrap_or_else(|| panic!("{case} prompt cache diagnostics unexpectedly succeeded"));
            assert!(
                error
                    .to_string()
                    .contains("prompt_cache_diagnostics was invalid"),
                "{case} prompt cache diagnostics returned an unhelpful diagnostic: {error:#}"
            );
        }
    }

    #[test]
    fn test_terminal_prompt_cache_options_and_retention_are_strict() {
        for (case, metadata) in [
            (
                "explicit-without-comparison",
                json!({"prompt_cache_options":{"mode":"explicit","ttl":"30m"}}),
            ),
            (
                "in-memory-retention",
                json!({"prompt_cache_retention":"in_memory"}),
            ),
            (
                "bounded-cache-key",
                json!({"prompt_cache_key":"k".repeat(256)}),
            ),
        ] {
            validate_documented_response_metadata(metadata.as_object().unwrap())
                .unwrap_or_else(|error| panic!("valid {case} metadata failed: {error:#}"));
        }

        for (case, metadata) in [
            (
                "invalid-mode",
                json!({"prompt_cache_options":{"mode":"future","ttl":"30m"}}),
            ),
            (
                "missing-ttl",
                json!({"prompt_cache_options":{"mode":"implicit"}}),
            ),
            (
                "unknown-option",
                json!({"prompt_cache_options":{"mode":"implicit","ttl":"30m","future":true}}),
            ),
            (
                "invalid-comparison-id",
                json!({"prompt_cache_options":{"mode":"implicit","ttl":"30m","comparison_response_id":""}}),
            ),
            (
                "invalid-retention",
                json!({"prompt_cache_retention":"forever"}),
            ),
            ("non-string-cache-key", json!({"prompt_cache_key":false})),
            (
                "oversized-cache-key",
                json!({"prompt_cache_key":"k".repeat(257)}),
            ),
        ] {
            let error = validate_documented_response_metadata(metadata.as_object().unwrap())
                .err()
                .unwrap_or_else(|| panic!("{case} prompt cache metadata unexpectedly succeeded"));
            assert!(
                error.to_string().contains("invalid")
                    || error.to_string().contains("unknown field")
                    || error.to_string().contains("exceeded the size limit"),
                "{case} prompt cache metadata returned an unhelpful diagnostic: {error:#}"
            );
        }
    }

    #[test]
    fn test_output_text_metadata_is_array_shaped_and_count_bounded() {
        for field in ["annotations", "logprobs"] {
            let mut accepted = json!({
                "type":"message",
                "role":"assistant",
                "content":[{"type":"output_text","text":"hello"}]
            });
            accepted["content"][0][field] = json!(vec![Value::Null; 256]);
            parse_output_item(
                &accepted,
                &mut Vec::new(),
                &mut HashSet::new(),
                &HashSet::new(),
            )
            .unwrap_or_else(|error| {
                panic!("{field} metadata at the inclusive count limit failed: {error:#}")
            });

            for (case, value) in [
                ("non-array", json!(false)),
                ("257 entries", json!(vec![Value::Null; 257])),
            ] {
                let mut rejected = json!({
                    "type":"message",
                    "role":"assistant",
                    "content":[{"type":"output_text","text":"hello"}]
                });
                rejected["content"][0][field] = value;
                let error = parse_output_item(
                    &rejected,
                    &mut Vec::new(),
                    &mut HashSet::new(),
                    &HashSet::new(),
                )
                .err()
                .unwrap_or_else(|| panic!("{case} {field} metadata unexpectedly succeeded"));
                assert!(
                    error.to_string().contains("metadata was invalid")
                        || error
                            .to_string()
                            .contains("metadata exceeded the size limit"),
                    "{case} {field} metadata returned an unhelpful diagnostic: {error:#}"
                );
            }
        }
    }

    #[test]
    fn test_event_obfuscation_must_be_string_padding() {
        let event = json!({"type":"response.created","obfuscation":{"future":true}});
        let error = exact_event_keys(
            event.as_object().unwrap(),
            &["type"],
            "response created event",
        )
        .err()
        .expect("structured obfuscation must not bypass event validation");
        assert!(
            error.to_string().contains("padding was invalid"),
            "structured obfuscation returned an unhelpful diagnostic: {error:#}"
        );
    }

    #[test]
    fn test_unknown_event_fields_report_only_static_location() {
        let sentinel = "sk-proj-SensitiveToken123";
        let event = json!({"type":"response.created",sentinel:null});
        let error = exact_event_keys(
            event.as_object().expect("event fixture must be an object"),
            &["type"],
            "response created event",
        )
        .err()
        .expect("unknown event semantics must remain fail closed");
        assert_eq!(
            error.to_string(),
            "ChatGPT subscription response created event contained an unknown field",
            "unknown event semantics must report only the static event location"
        );
        assert!(
            !error.to_string().contains(sentinel),
            "unknown event semantics reflected response-body field data"
        );
    }

    #[test]
    fn test_terminal_output_rejects_semantic_drift_after_passive_normalization() {
        let streamed = json!({
            "type":"message",
            "role":"assistant",
            "content":[{"type":"output_text","text":"hello","annotations":[],"logprobs":[]}]
        });
        let terminal = json!({
            "id":"resp-semantic-drift",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[{
                "type":"message",
                "role":"assistant",
                "content":[{"type":"output_text","text":"different"}]
            }]
        });
        let mut accumulator = StreamAccumulator::default();
        accumulator.output_items.insert(0, streamed);
        let error = parse_completed(
            terminal.as_object().unwrap(),
            DEFAULT_MODEL,
            Some(DEFAULT_MODEL),
            &HashSet::new(),
            &mut accumulator,
        )
        .err()
        .expect("terminal text drift must remain fail closed");
        assert!(
            error.to_string().contains(
                "terminal output item 0 (message) did not match streamed output item 0 \
                 (message); terminal_count=1, streamed_count=1"
            ),
            "terminal semantic drift returned an unhelpful diagnostic: {error:#}"
        );
    }

    #[test]
    fn malformed_unknown_and_misordered_terminal_events_fail_closed() {
        let mut accumulator = StreamAccumulator::default();
        let unknown = json!({"type":"response.future","sequence_number":1});
        assert!(parse_event(
            unknown,
            DEFAULT_MODEL,
            None,
            &HashSet::new(),
            &mut accumulator,
        )
        .is_err());
        let malformed = json!({
            "type":"response.output_text.delta",
            "sequence_number":1,
            "item_id":"item-1",
            "output_index":0,
            "content_index":0,
            "unexpected":"secret"
        });
        assert!(parse_event(
            malformed,
            DEFAULT_MODEL,
            None,
            &HashSet::new(),
            &mut accumulator,
        )
        .is_err());
        let terminal = json!({
            "id":"resp",
            "status":"in_progress",
            "model":DEFAULT_MODEL,
            "output":[]
        });
        assert!(parse_completed(
            terminal.as_object().unwrap(),
            DEFAULT_MODEL,
            None,
            &HashSet::new(),
            &mut accumulator,
        )
        .is_err());
    }

    #[test]
    fn test_metadata_events_accept_a_routed_model_and_reject_within_response_drift() {
        for kind in ["response.metadata", "codex.response.metadata"] {
            let mut accumulator = StreamAccumulator::default();
            let event = json!({
                "type":kind,
                "sequence_number":1,
                "response_id":"resp-1",
                "headers":{"OpenAI-Model":DEFAULT_MODEL},
                "metadata":{}
            });
            let parsed = parse_event(
                event,
                DEFAULT_MODEL,
                None,
                &HashSet::new(),
                &mut accumulator,
            )
            .unwrap_or_else(|error| panic!("{kind} rejected a valid model header: {error:#}"));
            assert!(
                parsed.is_none(),
                "{kind} unexpectedly completed a response; completed={}",
                parsed.is_some()
            );
            assert_eq!(
                accumulator.actual_model.as_deref(),
                Some(DEFAULT_MODEL),
                "{kind} did not retain its observed model; accumulator={:?}",
                accumulator.actual_model
            );
        }

        let mut accumulator = StreamAccumulator::default();
        let routed = json!({
            "type":"response.metadata",
            "sequence_number":1,
            "headers":{"openai-model":"gpt-4o"}
        });
        let parsed = parse_event(
            routed,
            DEFAULT_MODEL,
            None,
            &HashSet::new(),
            &mut accumulator,
        )
        .expect("a bounded serving-model identifier may differ from the requested route");
        assert!(
            parsed.is_none(),
            "routed model metadata unexpectedly completed a response; completed={}",
            parsed.is_some()
        );
        assert_eq!(
            accumulator.actual_model.as_deref(),
            Some("gpt-4o"),
            "routed model metadata was not retained; accumulator={:?}",
            accumulator.actual_model
        );

        let drift = json!({
            "type":"response.metadata",
            "sequence_number":2,
            "headers":{"openai-model":DEFAULT_MODEL}
        });
        let error = parse_event(
            drift,
            DEFAULT_MODEL,
            None,
            &HashSet::new(),
            &mut accumulator,
        )
        .err()
        .expect("contradictory model identities within one response must fail")
        .to_string();
        assert!(
            error.contains("changed during the response"),
            "model-identity drift returned an unhelpful diagnostic: {error}"
        );

        for (case, model) in [
            ("empty", String::new()),
            ("whitespace", "bad model".to_string()),
            ("over-limit", "m".repeat(257)),
        ] {
            let error = StreamAccumulator::default()
                .observe_model(&model)
                .err()
                .unwrap_or_else(|| panic!("{case} actual-model identifier was accepted"));
            assert!(
                error.to_string().contains("actual model was invalid"),
                "{case} actual-model identifier returned an unhelpful diagnostic: {error:#}"
            );
        }
    }

    #[test]
    fn test_outer_model_headers_accept_identical_duplicates_and_reject_drift() {
        let mut identical = reqwest::header::HeaderMap::new();
        identical.append("openai-model", "gpt-4o".parse().expect("valid header"));
        identical.append("openai-model", "gpt-4o".parse().expect("valid header"));
        let mut accumulator = StreamAccumulator::default();
        observe_outer_model_headers(&identical, &mut accumulator)
            .expect("identical outer model headers must remain valid");
        assert_eq!(
            accumulator.actual_model.as_deref(),
            Some("gpt-4o"),
            "identical duplicate headers did not retain their model; accumulator={:?}",
            accumulator.actual_model
        );

        let mut conflicting = identical;
        conflicting.append(
            "openai-model",
            DEFAULT_MODEL.parse().expect("valid default-model header"),
        );
        let mut accumulator = StreamAccumulator::default();
        let error = observe_outer_model_headers(&conflicting, &mut accumulator)
            .err()
            .expect("conflicting outer model headers must fail");
        assert!(
            error.to_string().contains("changed during the response"),
            "conflicting outer model headers returned an unhelpful diagnostic: {error:#}"
        );
    }

    #[test]
    fn production_refresh_lock_is_shared_by_named_credential_and_account() {
        let first = shared_refresh_lock("credential-a", "account-a");
        let second = shared_refresh_lock("credential-a", "account-a");
        let other_account = shared_refresh_lock("credential-a", "account-b");
        let other_credential = shared_refresh_lock("credential-b", "account-a");
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &other_account));
        assert!(!Arc::ptr_eq(&first, &other_credential));
    }

    #[tokio::test]
    async fn test_catalog_dispatch_uses_the_pinned_compatibility_version() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .match_header("authorization", "Bearer subscription-secret")
            .match_header("chatgpt-account-id", "account-1")
            .match_header("originator", "finch")
            .match_header("user-agent", FINCH_CHATGPT_USER_AGENT)
            .match_header("version", CHATGPT_CATALOG_CLIENT_VERSION)
            .with_status(200)
            .with_body(catalog_body())
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_body(completed_sse(DEFAULT_MODEL))
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .expect("catalog-version fixture must construct its provider");
        let outcome = provider
            .send_message(
                &ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]),
            )
            .await;
        models.assert_async().await;
        inference.assert_async().await;
        let response = outcome.unwrap_or_else(|error| {
            panic!("catalog request with pinned compatibility version failed: {error:#}")
        });
        assert_eq!(
            response.model, DEFAULT_MODEL,
            "catalog-version boundary changed terminal model provenance; response={response:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_accept_successful_sse_without_content_type() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("openai-model", DEFAULT_MODEL)
            .with_body(completed_sse(DEFAULT_MODEL))
            .expect(2)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .expect("missing-content-type fixture must construct a provider");
        let request = ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]);

        let (buffered, streaming) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );

        models.assert_async().await;
        inference.assert_async().await;
        let buffered = buffered.unwrap_or_else(|error| {
            panic!("valid headerless SSE failed through the buffered boundary: {error:#}")
        });
        assert_eq!(
            buffered.model, DEFAULT_MODEL,
            "headerless buffered SSE changed model provenance; response={buffered:?}"
        );
        let mut receiver = streaming.unwrap_or_else(|error| {
            panic!("valid headerless SSE failed through the streaming boundary: {error:#}")
        });
        let mut streamed_text = String::new();
        let mut terminal_metadata = None;
        while let Some(chunk) = receiver.recv().await {
            match chunk.unwrap_or_else(|error| {
                panic!("valid headerless SSE emitted a stream error: {error:#}")
            }) {
                StreamChunk::TextDelta(delta) => streamed_text.push_str(&delta),
                StreamChunk::ResponseMetadata { model } => terminal_metadata = Some(model),
                _ => {}
            }
        }
        assert_eq!(streamed_text, "hello", "headerless SSE lost streamed text");
        assert_eq!(
            terminal_metadata.as_deref(),
            Some(DEFAULT_MODEL),
            "headerless SSE omitted terminal model metadata"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_report_omitted_or_routed_model_provenance() {
        for (case, provenance, reported_model) in [
            ("omitted", None, DEFAULT_MODEL),
            ("routed", Some("gpt-4o"), "gpt-4o"),
        ] {
            let mut server = mockito::Server::new_async().await;
            let models = server
                .mock("GET", "/backend-api/codex/models")
                .match_query(mockito::Matcher::UrlEncoded(
                    "client_version".into(),
                    CHATGPT_CATALOG_CLIENT_VERSION.into(),
                ))
                .with_status(200)
                .with_body(catalog_body())
                .expect(1)
                .create_async()
                .await;
            let mut inference = server
                .mock("POST", RESPONSES_PATH)
                .with_status(200)
                .with_header("content-type", "text/event-stream")
                .with_body(completed_sse_with_model_provenance(provenance))
                .expect(2);
            if let Some(model) = provenance {
                inference = inference.with_header("openai-model", model);
            }
            let inference = inference.create_async().await;
            let provider = ChatGptSubscriptionProvider::for_test(
                Arc::new(StaticSource::new()),
                &format!("{}/backend-api/codex", server.url()),
                DEFAULT_MODEL,
            )
            .unwrap_or_else(|error| panic!("{case} model fixture failed to construct: {error:#}"));
            let request =
                ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]);

            let (buffered, streaming) = tokio::join!(
                provider.send_message(&request),
                provider.send_message_stream(&request)
            );

            models.assert_async().await;
            inference.assert_async().await;
            let buffered = buffered.unwrap_or_else(|error| {
                panic!("{case} model provenance failed through the buffered boundary: {error:#}")
            });
            assert_eq!(
                buffered.model, reported_model,
                "buffered response reported the wrong model for {case} provenance; \
                 response={buffered:?}"
            );
            assert!(
                buffered
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Text { text } if text == "hello")),
                "{case} model-provenance response lost buffered text; response={buffered:?}"
            );

            let mut receiver = streaming.unwrap_or_else(|error| {
                panic!("{case} model provenance failed through the streaming boundary: {error:#}")
            });
            let mut streamed_text = String::new();
            let mut terminal_model = None;
            while let Some(chunk) = receiver.recv().await {
                match chunk.unwrap_or_else(|error| {
                    panic!("{case} model provenance emitted a stream error: {error:#}")
                }) {
                    StreamChunk::TextDelta(delta) => streamed_text.push_str(&delta),
                    StreamChunk::ResponseMetadata { model } => terminal_model = Some(model),
                    _ => {}
                }
            }
            assert_eq!(
                streamed_text, "hello",
                "{case} model-provenance streaming response lost parsed text"
            );
            assert_eq!(
                terminal_model.as_deref(),
                Some(reported_model),
                "streaming response reported the wrong model for {case} provenance"
            );
        }
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_reject_malformed_model_provenance_before_terminal_effects()
    {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let malformed = format!(
            "event: response.created\ndata: {}\n\n",
            json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":"bad model"}}})
        );
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(malformed)
            .expect(2)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .expect("malformed-model-provenance fixture must construct a provider");
        let request = ProviderRequest::new(vec![Message::user("hello")]);

        let (buffered, streaming) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );

        models.assert_async().await;
        inference.assert_async().await;
        let buffered_error = buffered
            .err()
            .expect("malformed model provenance must fail the buffered response");
        assert!(
            buffered_error
                .to_string()
                .contains("actual model was invalid"),
            "malformed buffered provenance returned an unhelpful diagnostic: {buffered_error:#}"
        );

        let mut receiver = streaming
            .expect("stream setup must succeed before malformed SSE provenance is consumed");
        let mut outcome = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
        assert_eq!(
            outcome.len(),
            1,
            "malformed streaming provenance must emit exactly one terminal error and no effects; \
             outcome={outcome:?}"
        );
        assert!(
            matches!(&outcome[0], Err(error) if error.contains("actual model was invalid")),
            "malformed streaming provenance must emit its actionable terminal error and no effects; \
             outcome={outcome:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_accept_audited_passive_response_fields() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_body(completed_sse_with_audited_passive_fields(DEFAULT_MODEL))
            .expect(2)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .expect("audited-passive-field fixture must construct a provider");
        let request = ProviderRequest::new(vec![Message::user("hello")]);

        let (buffered, streaming) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );

        models.assert_async().await;
        inference.assert_async().await;
        let buffered = buffered.unwrap_or_else(|error| {
            panic!("audited passive fields failed through the buffered boundary: {error:#}")
        });
        assert_eq!(
            buffered.model, DEFAULT_MODEL,
            "audited passive fields changed buffered model provenance; response={buffered:?}"
        );
        assert!(
            buffered
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text } if text == "hello")),
            "audited passive fields lost buffered response text; response={buffered:?}"
        );

        let mut receiver = streaming.unwrap_or_else(|error| {
            panic!("audited passive fields failed through the streaming boundary: {error:#}")
        });
        let mut streamed_text = String::new();
        let mut terminal_metadata = None;
        while let Some(chunk) = receiver.recv().await {
            match chunk.unwrap_or_else(|error| {
                panic!("audited passive fields emitted a stream error: {error:#}")
            }) {
                StreamChunk::TextDelta(delta) => streamed_text.push_str(&delta),
                StreamChunk::ResponseMetadata { model } => terminal_metadata = Some(model),
                _ => {}
            }
        }
        assert_eq!(
            streamed_text, "hello",
            "audited passive fields lost streamed text"
        );
        assert_eq!(
            terminal_metadata.as_deref(),
            Some(DEFAULT_MODEL),
            "audited passive fields omitted terminal model metadata"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_accept_terminal_snapshot_omitting_streamed_reasoning() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_body(completed_sse_with_terminal_output(
                DEFAULT_MODEL,
                json!([{
                    "id":"message-terminal",
                    "type":"message",
                    "status":"completed",
                    "role":"assistant",
                    "phase":"final_answer",
                    "content":[{"type":"output_text","text":"hello"}]
                }]),
            ))
            .expect(2)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .expect("terminal-subset fixture must construct a provider");
        let request = ProviderRequest::new(vec![Message::user("hello")]);

        let (buffered, streaming) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );

        models.assert_async().await;
        inference.assert_async().await;
        let buffered = buffered.unwrap_or_else(|error| {
            panic!(
                "a terminal snapshot that omits already-validated streamed reasoning failed at \
                 the buffered provider boundary: {error:#}"
            )
        });
        assert!(
            buffered
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text } if text == "hello")),
            "terminal-subset reconciliation lost buffered text; response={buffered:?}"
        );
        assert!(
            buffered
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::OpaqueReasoning { .. })),
            "terminal-subset reconciliation lost the authoritative streamed reasoning item; \
             response={buffered:?}"
        );

        let mut receiver = streaming.unwrap_or_else(|error| {
            panic!(
                "a terminal snapshot that omits already-validated streamed reasoning failed \
                 before the streaming provider boundary: {error:#}"
            )
        });
        let mut outcome = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
        assert!(
            outcome.iter().all(Result::is_ok),
            "terminal-subset reconciliation emitted a streaming error after valid text; \
             outcome={outcome:?}"
        );
        assert!(
            outcome
                .iter()
                .any(|chunk| matches!(chunk, Ok(StreamChunk::TextDelta(text)) if text == "hello")),
            "terminal-subset reconciliation lost the streamed text delta; outcome={outcome:?}"
        );
        assert!(
            outcome.iter().any(|chunk| matches!(
                chunk,
                Ok(StreamChunk::ContentBlockComplete(
                    ContentBlock::OpaqueReasoning { .. }
                ))
            )),
            "terminal-subset reconciliation lost the completed streamed reasoning item; \
             outcome={outcome:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_accept_empty_terminal_output_after_streamed_message() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_body(completed_sse_with_streamed_message_and_empty_terminal_output(DEFAULT_MODEL))
            .expect(2)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .expect("empty-terminal-output fixture must construct a provider");
        let request = ProviderRequest::new(vec![Message::user("hello")]);

        let (buffered, streaming) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );

        models.assert_async().await;
        inference.assert_async().await;
        let buffered = buffered.unwrap_or_else(|error| {
            panic!(
                "an empty terminal output snapshot rejected a validated streamed message at the \
                 buffered provider boundary: {error:#}"
            )
        });
        assert!(
            matches!(
                buffered.content.as_slice(),
                [ContentBlock::Text { text }] if text == "hello"
            ),
            "empty-terminal-output reconciliation lost buffered text; response={buffered:?}"
        );
        assert_eq!(
            buffered.model, DEFAULT_MODEL,
            "empty-terminal-output reconciliation lost model metadata"
        );
        assert_eq!(
            buffered
                .usage
                .as_ref()
                .map(|usage| (usage.input_tokens, usage.output_tokens)),
            Some((12, 7)),
            "empty-terminal-output reconciliation lost terminal usage"
        );

        let mut receiver = streaming.unwrap_or_else(|error| {
            panic!(
                "an empty terminal output snapshot failed before the streaming provider \
                 boundary: {error:#}"
            )
        });
        let mut outcome = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
        assert!(
            matches!(
                outcome.as_slice(),
                [
                    Ok(StreamChunk::TextDelta(delta)),
                    Ok(StreamChunk::ResponseMetadata { model }),
                    Ok(StreamChunk::Usage {
                        input_tokens: 12,
                        output_tokens: 7,
                    }),
                    Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text })),
                ] if delta == "hello" && model == DEFAULT_MODEL && text == "hello"
            ),
            "empty-terminal-output reconciliation did not emit exactly one ordered text delta, \
             model identity, usage record, and completed message with no extra terminal effects; \
             outcome={outcome:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_reject_non_reasoning_terminal_snapshot_drift() {
        let reasoning = json!({
            "type":"reasoning",
            "summary":[],
            "encrypted_content":"opaque-1"
        });
        let message = json!({
            "type":"message",
            "role":"assistant",
            "content":[{"type":"output_text","text":"hello"}]
        });
        let function_call = json!({
            "type":"function_call",
            "call_id":"call-2",
            "name":"read",
            "namespace":"functions",
            "arguments":"{\"path\":\"README.md\"}"
        });

        assert_terminal_snapshot_rejected_at_provider_boundary(
            "omitted streamed function call",
            json!([reasoning.clone(), message.clone()]),
            "terminal snapshot omitted streamed output item 2 (function_call); \
             terminal_count=2, streamed_count=3",
        )
        .await;
        assert_terminal_snapshot_rejected_at_provider_boundary(
            "duplicate message masking a streamed function call",
            json!([reasoning.clone(), message.clone(), message.clone()]),
            "terminal output item 2 (message) did not match streamed output item 2 \
             (function_call); terminal_count=3, streamed_count=3",
        )
        .await;
        assert_terminal_snapshot_rejected_at_provider_boundary(
            "reordered message and function call",
            json!([reasoning, function_call, message]),
            "terminal output item 1 (function_call) did not match streamed output item 1 \
             (message); terminal_count=3, streamed_count=3",
        )
        .await;
    }

    #[tokio::test]
    async fn test_successful_sse_with_explicit_empty_content_type_is_rejected() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "")
            .with_body(completed_sse(DEFAULT_MODEL))
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .expect("empty-content-type fixture must construct a provider");

        let error = provider
            .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .expect_err("an explicit empty Content-Type was treated as an omitted header");

        models.assert_async().await;
        inference.assert_async().await;
        assert!(
            error.to_string().contains("not an event stream"),
            "explicit empty Content-Type returned the wrong diagnostic: {error:#}"
        );
    }

    #[tokio::test]
    async fn test_buffered_inference_uses_the_pinned_compatibility_version() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                "0.151.0".into(),
            ))
            .match_header("authorization", "Bearer subscription-secret")
            .match_header("chatgpt-account-id", "account-1")
            .match_header("originator", "finch")
            .match_header("user-agent", FINCH_CHATGPT_USER_AGENT)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_header("etag", "account-etag")
            .with_body(catalog_body())
            .create_async()
            .await;
        let expected = json!({
            "model": DEFAULT_MODEL,
            "input": [
                {
                    "id": "at_06f0d744-9e74-54f8-9371-312adc3c666b",
                    "type": "additional_tools",
                    "role": "developer",
                    "tools": [{
                        "type": "namespace",
                        "name": "functions",
                        "description": "",
                        "tools": [{
                            "type": "function",
                            "name": "read",
                            "description": "Read a file",
                            "strict": false,
                            "parameters": {
                                "type": "object",
                                "properties": {"path":{"type":"string"}},
                                "required": ["path"],
                                "additionalProperties": false
                            }
                        }]
                    }]
                },
                {"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "reasoning": {"effort":"high","context":"all_turns"},
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"]
        });
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .match_header("authorization", "Bearer subscription-secret")
            .match_header("chatgpt-account-id", "account-1")
            .match_header("originator", "finch")
            .match_header("user-agent", FINCH_CHATGPT_USER_AGENT)
            .match_header("version", CHATGPT_CATALOG_CLIENT_VERSION)
            .match_header("x-openai-internal-codex-responses-lite", "true")
            .match_body(mockito::Matcher::Json(expected))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_header("x-codex-primary-used-percent", "25.5")
            .with_body(completed_sse(DEFAULT_MODEL))
            .create_async()
            .await;
        let source = Arc::new(StaticSource::new());
        let provider = ChatGptSubscriptionProvider::for_test(
            source.clone(),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap();
        let outcome = provider
            .send_message(
                &ProviderRequest::new(vec![Message::user("hello")])
                    .with_model(DEFAULT_MODEL)
                    .with_tools(vec![tool()]),
            )
            .await;
        models.assert_async().await;
        inference.assert_async().await;
        let response = outcome.unwrap_or_else(|error| {
            panic!("buffered inference with pinned compatibility version failed: {error:#}")
        });
        assert_eq!(response.model, DEFAULT_MODEL);
        assert_eq!(response.usage.unwrap().output_tokens, 7);
        assert_eq!(response.allowance.unwrap().primary_used_percent, Some(25.5));
        assert!(matches!(
            response.content.first(),
            Some(ContentBlock::OpaqueReasoning { encrypted_content }) if encrypted_content == "opaque-1"
        ));
        assert!(matches!(
            response.content.last(),
            Some(ContentBlock::ToolUse { id, .. }) if id == "call-2"
        ));
        assert_eq!(source.refreshes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_streaming_inference_uses_the_pinned_compatibility_version() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(single_model_catalog_body(DEFAULT_MODEL, 1_000_000))
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .match_header("version", CHATGPT_CATALOG_CLIENT_VERSION)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_body(completed_sse(DEFAULT_MODEL))
            .expect(2)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap();
        let request = ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]);
        let (nonstream, stream) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );
        models.assert_async().await;
        inference.assert_async().await;
        let nonstream = nonstream.unwrap_or_else(|error| {
            panic!("buffered half of streaming parity dispatch failed: {error:#}")
        });
        let mut receiver = stream.unwrap_or_else(|error| {
            panic!("streaming inference with pinned compatibility version failed: {error:#}")
        });
        let mut streamed_blocks = Vec::new();
        let mut streamed_model = None;
        let mut streamed_usage = None;
        let mut streamed_text = String::new();
        while let Some(chunk) = receiver.recv().await {
            match chunk.unwrap() {
                StreamChunk::TextDelta(delta) => streamed_text.push_str(&delta),
                StreamChunk::ContentBlockComplete(block) => streamed_blocks.push(block),
                StreamChunk::ResponseMetadata { model } => streamed_model = Some(model),
                StreamChunk::Usage {
                    input_tokens,
                    output_tokens,
                } => streamed_usage = Some((input_tokens, output_tokens)),
                _ => {}
            }
        }
        assert_eq!(
            serde_json::to_value(streamed_blocks).unwrap(),
            serde_json::to_value(&nonstream.content).unwrap()
        );
        assert_eq!(streamed_model.as_deref(), Some(nonstream.model.as_str()));
        assert_eq!(streamed_usage, Some((12, 7)));
        assert_eq!(streamed_text, "hello");
    }

    #[tokio::test]
    async fn buffered_and_streaming_require_the_exact_requested_catalog_entry() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(single_model_catalog_body(MODEL_ALIAS, 1_000_000))
            .expect(1)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap();
        let request = ProviderRequest::new(vec![Message::user("hello")]);

        let buffered_error = provider
            .send_message(&request)
            .await
            .err()
            .expect("buffered request must require its exact catalog entry");
        assert!(buffered_error.is::<SubscriptionRequestedModelUnavailable>());
        assert!(!buffered_error.to_string().contains("subscription-secret"));
        assert!(!buffered_error.to_string().contains("account-1"));

        let streaming_error = provider
            .send_message_stream(&request)
            .await
            .err()
            .expect("streaming request must require its exact catalog entry");
        assert!(streaming_error.is::<SubscriptionRequestedModelUnavailable>());
        assert!(!streaming_error.to_string().contains("subscription-secret"));
        assert!(!streaming_error.to_string().contains("account-1"));
        models.assert_async().await;
    }

    #[tokio::test]
    async fn byte_fragmented_sse_preserves_ordered_opaque_and_tool_items() {
        let base = fragmented_subscription_server(completed_sse(DEFAULT_MODEL)).await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &base,
            DEFAULT_MODEL,
        )
        .unwrap();
        let response = provider
            .send_message(
                &ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]),
            )
            .await
            .unwrap();
        assert!(matches!(
            response.content.as_slice(),
            [
                ContentBlock::OpaqueReasoning { .. },
                ContentBlock::Text { .. },
                ContentBlock::ToolUse { .. }
            ]
        ));
    }

    #[tokio::test]
    async fn one_pre_stream_unauthorized_refreshes_same_account_once() {
        let mut server = mockito::Server::new_async().await;
        let initial_catalog = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .match_header("authorization", "Bearer subscription-secret")
            .with_status(200)
            .with_body(catalog_body())
            .create_async()
            .await;
        let refreshed_catalog = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .match_header("authorization", "Bearer refreshed-subscription-secret")
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let first = server
            .mock("POST", RESPONSES_PATH)
            .match_header("authorization", "Bearer subscription-secret")
            .with_status(401)
            .with_body("do-not-log-this-secret")
            .expect(1)
            .create_async()
            .await;
        let second = server
            .mock("POST", RESPONSES_PATH)
            .match_header("authorization", "Bearer refreshed-subscription-secret")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_body(completed_sse(DEFAULT_MODEL))
            .expect(1)
            .create_async()
            .await;
        let source = Arc::new(StaticSource::new());
        let provider = ChatGptSubscriptionProvider::for_test(
            source.clone(),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap();
        provider
            .send_message(
                &ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]),
            )
            .await
            .unwrap();
        assert_eq!(source.refreshes.load(Ordering::SeqCst), 1);
        initial_catalog.assert_async().await;
        refreshed_catalog.assert_async().await;
        first.assert_async().await;
        second.assert_async().await;
    }

    #[tokio::test]
    async fn one_catalog_unauthorized_refreshes_before_inference_only_once() {
        let mut server = mockito::Server::new_async().await;
        let rejected_catalog = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .match_header("authorization", "Bearer subscription-secret")
            .with_status(401)
            .with_body("catalog-auth-secret")
            .expect(1)
            .create_async()
            .await;
        let refreshed_catalog = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .match_header("authorization", "Bearer refreshed-subscription-secret")
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .match_header("authorization", "Bearer refreshed-subscription-secret")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_body(completed_sse(DEFAULT_MODEL))
            .expect(1)
            .create_async()
            .await;
        let source = Arc::new(StaticSource::new());
        let provider = ChatGptSubscriptionProvider::for_test(
            source.clone(),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap();
        provider
            .send_message(
                &ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]),
            )
            .await
            .unwrap();
        assert_eq!(source.refreshes.load(Ordering::SeqCst), 1);
        rejected_catalog.assert_async().await;
        refreshed_catalog.assert_async().await;
        inference.assert_async().await;
    }

    #[tokio::test]
    async fn stream_receiver_drop_cancels_and_releases_subscription_transport() {
        let (base, closed) = stalling_subscription_server(true).await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &base,
            DEFAULT_MODEL,
        )
        .unwrap();
        let receiver = provider
            .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .unwrap();
        drop(receiver);
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("subscription transport was not released after receiver drop")
            .unwrap();
    }

    #[tokio::test]
    async fn caller_cancellation_reaches_and_releases_subscription_transport() {
        let (base, closed) = stalling_subscription_server(true).await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &base,
            DEFAULT_MODEL,
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let mut receiver = provider
            .send_message_stream(
                &ProviderRequest::new(vec![Message::user("hello")])
                    .with_cancellation_token(cancel.clone()),
            )
            .await
            .unwrap();
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("cancelled subscription stream did not terminate")
            .expect("cancelled subscription stream omitted its error")
            .unwrap_err()
            .to_string();
        assert!(error.contains("cancelled"));
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("subscription transport was not released after caller cancellation")
            .unwrap();
    }

    #[tokio::test]
    async fn request_timeout_is_bounded_and_releases_subscription_transport() {
        let (base, closed) = stalling_subscription_server(false).await;
        let mut provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &base,
            DEFAULT_MODEL,
        )
        .unwrap();
        provider.client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let error = provider
            .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Failed to start ChatGPT subscription response"));
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("subscription transport was not released after request timeout")
            .unwrap();
    }

    #[tokio::test]
    async fn test_eof_duplicate_done_and_post_terminal_data_fail_before_completion_effects() {
        let created = format!(
            "event: response.created\ndata: {}\n\n",
            json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":DEFAULT_MODEL}}})
        );
        let eof = subscription_stream_outcome(created.clone(), DEFAULT_MODEL).await;
        assert_eq!(
            eof.len(),
            1,
            "EOF before completion must emit exactly one terminal error and no completion effects; \
             outcome={eof:?}"
        );
        assert!(
            matches!(&eof[0], Err(error) if error.contains("before response.completed")),
            "EOF before completion must report the missing response.completed event; \
             outcome={eof:?}"
        );

        let terminal = format!(
            "{}event: response.completed\ndata: {}\n\ndata: [DONE]\n\n",
            created,
            json!({"type":"response.completed","sequence_number":2,"response":{"id":"resp"}})
        );
        let duplicate =
            subscription_stream_outcome(format!("{terminal}data: [DONE]\n\n"), DEFAULT_MODEL).await;
        assert_eq!(
            duplicate.len(),
            1,
            "duplicate terminal markers must emit exactly one terminal error and no completion \
             effects; outcome={duplicate:?}"
        );
        assert!(
            matches!(&duplicate[0], Err(error) if error.contains("terminal marker was invalid")),
            "duplicate terminal markers must report an invalid terminal marker; \
             outcome={duplicate:?}"
        );

        let late = subscription_stream_outcome(
            format!(
                "{terminal}event: response.in_progress\ndata: {}\n\n",
                json!({"type":"response.in_progress","sequence_number":3,"response":{}})
            ),
            DEFAULT_MODEL,
        )
        .await;
        assert_eq!(
            late.len(),
            1,
            "post-terminal data must emit exactly one terminal error and no completion effects; \
             outcome={late:?}"
        );
        assert!(
            matches!(&late[0], Err(error) if error.contains("data after its terminal response")),
            "post-terminal data must report data after the terminal response; outcome={late:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_reject_actual_model_drift_before_completion_effects() {
        let body = format!(
            concat!(
                "event: response.created\ndata: {}\n\n",
                "event: response.metadata\ndata: {}\n\n"
            ),
            json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":"gpt-4o"}}}),
            json!({"type":"response.metadata","sequence_number":2,"headers":{"openai-model":DEFAULT_MODEL}})
        );
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", "gpt-4o")
            .with_body(body)
            .expect(2)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .expect("model-drift fixture must construct a provider");
        let request = ProviderRequest::new(vec![Message::user("hello")]);
        let (buffered, streaming) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );
        models.assert_async().await;
        inference.assert_async().await;

        let buffered_error = buffered
            .err()
            .expect("model drift must fail the buffered response");
        assert!(
            buffered_error
                .to_string()
                .contains("changed during the response"),
            "buffered model drift returned an unhelpful diagnostic: {buffered_error:#}"
        );
        let mut receiver = streaming
            .expect("stream setup must succeed before contradictory provenance is consumed");
        let mut outcome = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
        assert_eq!(
            outcome.len(),
            1,
            "model drift must emit exactly one terminal error and no completion effects; \
             outcome={outcome:?}"
        );
        assert!(
            matches!(&outcome[0], Err(error) if error.contains("changed during the response")),
            "model drift must emit exactly one terminal error and no completion effects; \
             outcome={outcome:?}"
        );
    }

    #[tokio::test]
    async fn test_buffered_and_streaming_reject_conflicting_outer_model_headers_before_effects() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", "gpt-4o")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_body(completed_sse_with_model_provenance(None))
            .expect(2)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .expect("conflicting-outer-header fixture must construct a provider");
        let request = ProviderRequest::new(vec![Message::user("hello")]);
        let (buffered, streaming) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );
        models.assert_async().await;
        inference.assert_async().await;

        let buffered_error = buffered
            .err()
            .expect("conflicting outer model headers must fail the buffered response");
        assert!(
            buffered_error
                .to_string()
                .contains("changed during the response"),
            "conflicting buffered outer headers returned an unhelpful diagnostic: \
             {buffered_error:#}"
        );
        let mut receiver = streaming.expect(
            "stream setup must succeed before conflicting outer model headers are consumed",
        );
        let mut outcome = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
        assert_eq!(
            outcome.len(),
            1,
            "conflicting outer model headers must emit exactly one terminal error and no effects; \
             outcome={outcome:?}"
        );
        assert!(
            matches!(&outcome[0], Err(error) if error.contains("changed during the response")),
            "conflicting outer model headers returned the wrong streaming outcome; \
             outcome={outcome:?}"
        );
    }

    #[tokio::test]
    async fn catalog_etag_is_generation_revalidated_but_never_crosses_accounts() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .match_header("chatgpt-account-id", "account-1")
            .match_header("if-none-match", mockito::Matcher::Missing)
            .with_status(200)
            .with_header("etag", "account-1-etag")
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let revalidated = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .match_header("chatgpt-account-id", "account-1")
            .match_header("if-none-match", "account-1-etag")
            .with_status(304)
            .expect(1)
            .create_async()
            .await;
        let other_account = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .match_header("chatgpt-account-id", "account-2")
            .match_header("if-none-match", mockito::Matcher::Missing)
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap();
        let cancel = CancellationToken::new();
        provider
            .account_catalog(
                &ChatGptCredentialLease {
                    access_token: "secret-1".into(),
                    account: "account-1".into(),
                    generation: "generation-1".into(),
                },
                &cancel,
            )
            .await
            .unwrap();
        provider
            .account_catalog(
                &ChatGptCredentialLease {
                    access_token: "secret-2".into(),
                    account: "account-1".into(),
                    generation: "generation-2".into(),
                },
                &cancel,
            )
            .await
            .unwrap();
        provider
            .account_catalog(
                &ChatGptCredentialLease {
                    access_token: "secret-3".into(),
                    account: "account-2".into(),
                    generation: "generation-3".into(),
                },
                &cancel,
            )
            .await
            .unwrap();
        first.assert_async().await;
        revalidated.assert_async().await;
        other_account.assert_async().await;
    }

    #[tokio::test]
    async fn non_success_bodies_are_bounded_and_redacted() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .create_async()
            .await;
        let secret = "attacker-tool-argument-and-reasoning-secret";
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(429)
            .with_body(format!("{secret}{}", "x".repeat(MAX_ERROR_BYTES)))
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap();
        let error = provider
            .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains(secret));
        assert!(!error.contains("subscription-secret"));
        assert!(error.contains("size limit"));
        assert!(error.len() < 256);
        models.assert_async().await;
        inference.assert_async().await;
    }

    #[tokio::test]
    async fn response_rejection_is_typed_clear_and_secret_free() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .create_async()
            .await;
        let attacker_body = "account-1 subscription-secret private-tool-argument private-reasoning";
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(400)
            .with_body(attacker_body)
            .create_async()
            .await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap();
        let error = provider
            .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .unwrap_err();
        let rejection = error
            .downcast_ref::<SubscriptionResponseRejected>()
            .expect("HTTP rejection must retain its typed provider boundary");
        assert_eq!(rejection.0, StatusCode::BAD_REQUEST);
        let display = error.to_string();
        assert!(display.contains("HTTP 400 Bad Request"));
        assert!(display.contains("pinned protocol contract may have changed"));
        assert!(!display.contains(attacker_body));
        assert!(!display.contains("account-1"));
        assert!(!display.contains("subscription-secret"));
        assert!(display.len() < 256);
        models.assert_async().await;
        inference.assert_async().await;
    }

    #[test]
    fn hostile_origin_route_and_request_preflight_fail_before_credentials() {
        let source = Arc::new(StaticSource::new());
        for endpoint in [
            "https://api.openai.com/backend-api/codex",
            "https://chatgpt.com.evil/backend-api/codex",
            "https://user@chatgpt.com/backend-api/codex",
            "https://chatgpt.com/backend-api/codex?redirect=evil",
        ] {
            assert!(ChatGptSubscriptionProvider::new(
                source.clone(),
                endpoint,
                DEFAULT_MODEL,
                ReasoningEffort::High,
                false,
            )
            .is_err());
        }
        assert_eq!(source.leases.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn malformed_image_and_history_fail_before_credential_or_network_use() {
        let source = Arc::new(StaticSource::new());
        let provider = ChatGptSubscriptionProvider::for_test(
            source.clone(),
            "http://127.0.0.1:9/backend-api/codex",
            DEFAULT_MODEL,
        )
        .unwrap();
        let invalid_image = ProviderRequest::new(vec![Message::with_content(
            "user",
            vec![ContentBlock::image("image/png", "iVBORw0KGgo=")],
        )]);
        assert!(provider.send_message(&invalid_image).await.is_err());
        let invalid_role = ProviderRequest::new(vec![Message::with_content(
            "developer",
            vec![ContentBlock::text("attacker-controlled")],
        )]);
        assert!(provider.send_message(&invalid_role).await.is_err());
        assert_eq!(source.leases.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn tool_argument_reasoning_and_sse_boundaries_are_enforced() {
        let huge = "x".repeat(MAX_TOOL_ARGUMENT_BYTES + 1);
        let request = ProviderRequest::new(vec![Message::with_content(
            "assistant",
            vec![ContentBlock::ToolUse {
                id: "call".to_string(),
                name: "read".to_string(),
                input: json!({"secret":huge}),
            }],
        )])
        .with_model(DEFAULT_MODEL);
        assert!(responses_lite_request(&request, ReasoningEffort::High).is_err());
        assert!(enforce_sse_remainder_bounds(&vec![b'x'; MAX_SSE_LINE_BYTES + 1]).is_err());
        assert!(sse_data(b"future: attacker-secret").is_err());
    }

    /// Opt-in live acceptance uses Finch's own named device credential. It is
    /// ignored by default and intentionally never prints tokens or bodies.
    #[tokio::test]
    #[ignore = "requires FINCH_LIVE_CHATGPT_ACCEPTANCE=1 and reviewed Finch device login"]
    async fn live_chatgpt_subscription_sol_acceptance_is_explicitly_opt_in() -> Result<()> {
        if std::env::var("FINCH_LIVE_CHATGPT_ACCEPTANCE").as_deref() != Ok("1") {
            bail!("Set FINCH_LIVE_CHATGPT_ACCEPTANCE=1 after security review");
        }
        let config_path = std::env::var_os("FINCH_LIVE_CHATGPT_CONFIG")
            .context("Set FINCH_LIVE_CHATGPT_CONFIG to Finch's config.toml")?;
        let config = crate::config::load_config_from_path(std::path::Path::new(&config_path))?;
        let (binding, configured_model, configured_reasoning) = config
            .providers
            .iter()
            .find_map(|entry| match entry {
                crate::config::ProviderEntry::Credentialed {
                    provider: CredentialProvider::ChatgptSubscription,
                    credential,
                    model,
                    reasoning_effort,
                    ..
                } => Some((credential, model.as_deref(), *reasoning_effort)),
                _ => None,
            })
            .context("No Finch ChatGPT subscription profile is configured")?;
        let credential = config
            .credentials
            .iter()
            .find(|credential| credential.name == binding.credential_ref)
            .context("Finch ChatGPT subscription profile references a missing credential")?;
        let oauth_root = std::env::var_os("FINCH_LIVE_CHATGPT_OAUTH_ROOT")
            .context("Set FINCH_LIVE_CHATGPT_OAUTH_ROOT to Finch's oauth directory")?;
        let provider = ChatGptSubscriptionProvider::production_in_oauth_root(
            credential,
            configured_model,
            configured_reasoning,
            oauth_root,
        )?;
        let response = provider
            .send_message(
                &ProviderRequest::new(vec![Message::user(
                    "Reply with exactly: Finch native subscription transport accepted",
                )])
                .with_model(DEFAULT_MODEL),
            )
            .await?;
        if response.model != DEFAULT_MODEL || response.text().is_empty() {
            bail!("Live ChatGPT subscription acceptance returned incompatible provenance");
        }
        Ok::<(), anyhow::Error>(())
    }
}
