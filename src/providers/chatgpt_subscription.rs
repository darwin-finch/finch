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
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
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
const DEFAULT_MODEL: &str = "gpt-5.6-sol";
const MODEL_ALIAS: &str = "gpt-5.6";
const RESPONSES_PATH: &str = "/backend-api/codex/responses";
const MODELS_PATH: &str = "/backend-api/codex/models";
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;
const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;
const MAX_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_OPAQUE_REASONING_BYTES: usize = 4 * 1024 * 1024;
const MAX_OUTPUT_ITEMS: usize = 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const CATALOG_TTL: Duration = Duration::from_secs(5 * 60);
const REFRESH_SKEW: ChronoDuration = ChronoDuration::minutes(2);

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
    refresh_lock: Mutex<()>,
}

impl ProductionCredentialSource {
    fn new(credential: &ProviderCredential) -> Result<Self> {
        validate_configured_credential(credential)?;
        let reference = credential
            .secret_ref
            .strip_prefix("oauth-store:")
            .context("ChatGPT subscription credential has an incompatible secret reference")?;
        if reference != credential.name {
            bail!("ChatGPT subscription credential reference changed identity");
        }
        let root = dirs::home_dir()
            .context("Could not determine Finch credential store location")?
            .join(".finch")
            .join("oauth");
        let store = Arc::new(FileOAuthCredentialStore::new(root));
        let dialect = Arc::new(OpenAiChatGptOAuthDialect::production()?);
        let oauth = Arc::new(OAuthClient::new(dialect, store.clone())?);
        Ok(Self {
            reference: reference.to_string(),
            expected_account: credential
                .account
                .clone()
                .context("ChatGPT subscription credential omitted its signed account")?,
            store,
            oauth,
            refresh_lock: Mutex::new(()),
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
        let _guard = self.refresh_lock.lock().await;
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
            reasoning_effort.unwrap_or(ReasoningEffort::High),
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
        let mut cache = self.catalog.lock().await;
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
            Some(("client_version", env!("CARGO_PKG_VERSION"))),
        )?;
        let mut request = self
            .client
            .get(url)
            .bearer_auth(&lease.access_token)
            .header("ChatGPT-Account-ID", &lease.account)
            .header("originator", "finch")
            .header("version", env!("CARGO_PKG_VERSION"))
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
        let mut lease = self.source.lease(&cancel).await?;
        let catalog = self.account_catalog(&lease, &cancel).await?;
        let selected = catalog
            .models
            .get(&request.model)
            .context("ChatGPT account does not advertise the configured model")?;
        if !selected.responses_lite || selected.slug != request.model {
            bail!("ChatGPT account model is not compatible with the pinned Responses-Lite dialect");
        }
        let body = responses_lite_request(&request, self.reasoning_effort)?;
        let body =
            serde_json::to_vec(&body).context("Failed to encode ChatGPT subscription request")?;
        if body.len() > MAX_REQUEST_BYTES {
            bail!("ChatGPT subscription request exceeded the size limit");
        }
        let url = self.route(RESPONSES_PATH, None)?;
        for attempt in 0..2 {
            let response = tokio::select! {
                _ = cancel.cancelled() => bail!("ChatGPT subscription request was cancelled"),
                response = self.client.post(url.clone())
                    .bearer_auth(&lease.access_token)
                    .header("ChatGPT-Account-ID", &lease.account)
                    .header("originator", "finch")
                    .header("version", env!("CARGO_PKG_VERSION"))
                    .header("x-finch-chatgpt-protocol", CHATGPT_INFERENCE_PROTOCOL_REVISION)
                    .header("x-openai-internal-codex-responses-lite", "true")
                    .header(reqwest::header::ACCEPT, "text/event-stream")
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send() => response.context("Failed to start ChatGPT subscription response")?,
            };
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                let _ = read_bounded(response, MAX_ERROR_BYTES, &cancel).await?;
                lease = self
                    .source
                    .refresh_after_unauthorized(&lease.generation, &cancel)
                    .await?;
                if !catalog.models.contains_key(&request.model) {
                    bail!("ChatGPT account model changed while refreshing credentials");
                }
                continue;
            }
            if !response.status().is_success() {
                let status = response.status();
                let _ = read_bounded(response, MAX_ERROR_BYTES, &cancel).await?;
                bail!("ChatGPT subscription response failed (HTTP {status})");
            }
            let content_type =
                bounded_header(response.headers(), reqwest::header::CONTENT_TYPE.as_str())?
                    .unwrap_or_default();
            if !content_type.starts_with("text/event-stream") {
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
        let cancel = CancellationToken::new();
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
        Some(1_050_000),
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
        bail!("ChatGPT GPT-5.6 Sol reasoning effort is unsupported");
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
    let mut input = Vec::new();
    input.push(json!({"type":"additional_tools","role":"developer","tools":tools}));
    if let Some(system) = request.system.as_deref().filter(|value| !value.is_empty()) {
        validate_bounded_text(system, MAX_TOOL_ARGUMENT_BYTES, "instructions")?;
        input.push(json!({
            "type":"message","role":"developer",
            "content":[{"type":"input_text","text":system}]
        }));
    }
    let mut calls = HashSet::new();
    let mut results = HashSet::new();
    for message in &request.messages {
        map_message(message, &mut input, &mut calls, &mut results)?;
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
                if !calls.insert(id.clone()) {
                    bail!("ChatGPT subscription history repeated a tool call identifier");
                }
                let arguments = serde_json::to_string(arguments)
                    .context("Failed to serialize ChatGPT function arguments")?;
                validate_bounded_text(&arguments, MAX_TOOL_ARGUMENT_BYTES, "tool arguments")?;
                input.push(json!({
                    "type":"function_call","call_id":id,"name":name,"arguments":arguments
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
    )?;
    let models = root["models"]
        .as_array()
        .context("ChatGPT catalog omitted models")?;
    if models.is_empty() || models.len() > 512 {
        bail!("ChatGPT subscription model catalog was empty or excessive");
    }
    let mut parsed = BTreeMap::new();
    for model in models {
        let object = model
            .as_object()
            .context("ChatGPT catalog model was invalid")?;
        let slug = required_identifier(object, "slug", 256)?;
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
        let context_window = object
            .get("context_window")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .context("ChatGPT catalog model omitted a valid context window")?;
        if context_window == 0 || context_window > 4_000_000 {
            bail!("ChatGPT catalog model context window was invalid");
        }
        if supported && matches!(slug.as_str(), DEFAULT_MODEL | MODEL_ALIAS) {
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
    if !parsed.contains_key(DEFAULT_MODEL) {
        bail!("ChatGPT account does not advertise gpt-5.6-sol");
    }
    Ok(Catalog { models: parsed })
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct Allowance {
    primary_used_percent: Option<u8>,
    secondary_used_percent: Option<u8>,
}

async fn consume_sse(
    response: Response,
    sender: Option<mpsc::Sender<Result<StreamChunk>>>,
    cancel: CancellationToken,
    expected_model: String,
    allowed_tools: HashSet<String>,
) -> Result<CompletedResponse> {
    let header_model = bounded_header(response.headers(), "openai-model")?;
    let header_allowance = parse_allowance_headers(response.headers())?;
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
            let data = sse_data(&event)?;
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
            )? {
                terminal = Some(completed);
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
            exact_keys(object, &["type", "sequence_number", "response"])?;
            required_sequence(object)?;
            object
                .get("response")
                .and_then(Value::as_object)
                .context("ChatGPT response lifecycle event omitted response")?;
            Ok(None)
        }
        "response.output_item.added" | "response.output_item.done" => {
            exact_keys(object, &["type", "sequence_number", "output_index", "item"])?;
            required_sequence(object)?;
            required_index(object, "output_index")?;
            validate_output_item_shape(
                object
                    .get("item")
                    .context("ChatGPT output item event omitted item")?,
            )?;
            Ok(None)
        }
        "response.content_part.added" | "response.content_part.done" => {
            exact_keys(
                object,
                &[
                    "type",
                    "sequence_number",
                    "item_id",
                    "output_index",
                    "content_index",
                    "part",
                ],
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
            validate_text_event(object, "delta", true)?;
            Ok(None)
        }
        "response.output_text.done" => {
            validate_text_event(object, "text", true)?;
            Ok(None)
        }
        "response.function_call_arguments.delta" => {
            validate_text_event(object, "delta", false)?;
            Ok(None)
        }
        "response.function_call_arguments.done" => {
            validate_text_event(object, "arguments", false)?;
            Ok(None)
        }
        "response.completed" => {
            exact_keys(object, &["type", "sequence_number", "response"])?;
            required_sequence(object)?;
            let response = object
                .get("response")
                .and_then(Value::as_object)
                .context("ChatGPT completion omitted response")?;
            parse_completed(response, expected_model, header_model, allowed_tools).map(Some)
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
) -> Result<()> {
    let mut keys = vec!["type", "sequence_number", "item_id", "output_index", field];
    if has_content_index {
        keys.extend(["content_index", "logprobs"]);
    }
    exact_keys(object, &keys)?;
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

fn parse_completed(
    response: &Map<String, Value>,
    expected_model: &str,
    header_model: Option<&str>,
    allowed_tools: &HashSet<String>,
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
        ],
    )?;
    if response.get("status").and_then(Value::as_str) != Some("completed") {
        bail!("ChatGPT terminal response status was invalid");
    }
    let id = required_identifier(response, "id", 256)?;
    let response_model = required_identifier(response, "model", 256)?;
    if let Some(header_model) = header_model {
        validate_identifier(header_model, 256, "actual model")?;
        if header_model != response_model {
            bail!("ChatGPT subscription actual model changed during the response");
        }
    }
    if !model_is_compatible(expected_model, &response_model) {
        bail!("ChatGPT subscription returned an incompatible actual model");
    }
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .context("ChatGPT completion omitted output items")?;
    if output.len() > MAX_OUTPUT_ITEMS {
        bail!("ChatGPT completion returned too many output items");
    }
    let mut blocks = Vec::new();
    let mut call_ids = HashSet::new();
    for item in output {
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
                &["id", "type", "status", "role", "content", "phase"],
            )?;
            if object.get("role").and_then(Value::as_str) != Some("assistant") {
                bail!("ChatGPT response message role was invalid");
            }
            let content = object
                .get("content")
                .and_then(Value::as_array)
                .context("ChatGPT response message omitted content")?;
            for part in content {
                let part = part
                    .as_object()
                    .context("ChatGPT response content was invalid")?;
                if part.get("type").and_then(Value::as_str) != Some("output_text") {
                    bail!("ChatGPT response contained an unknown message content type");
                }
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .context("ChatGPT output text was invalid")?;
                validate_bounded_text(text, MAX_RESPONSE_BYTES, "output text")?;
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
                ],
            )?;
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
                ],
            )?;
            let call_id = required_identifier(object, "call_id", 256)?;
            let name = required_identifier(object, "name", 128)?;
            if !allowed_tools.contains(&name) {
                bail!("ChatGPT requested a function Finch did not advertise");
            }
            let arguments = object
                .get("arguments")
                .and_then(Value::as_str)
                .context("ChatGPT function call omitted arguments")?;
            validate_bounded_text(arguments, MAX_TOOL_ARGUMENT_BYTES, "tool arguments")?;
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

fn parse_usage(value: Option<&Value>) -> Result<(Option<u32>, Option<u32>)> {
    let Some(value) = value else {
        return Ok((None, None));
    };
    let object = value
        .as_object()
        .context("ChatGPT response usage was invalid")?;
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
    Ok((convert("input_tokens")?, convert("output_tokens")?))
}

fn parse_allowance_headers(headers: &reqwest::header::HeaderMap) -> Result<Option<Allowance>> {
    let parse = |name: &str| -> Result<Option<u8>> {
        bounded_header(headers, name)?
            .map(|value| {
                value
                    .parse::<u8>()
                    .ok()
                    .filter(|value| *value <= 100)
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

fn model_is_compatible(requested: &str, actual: &str) -> bool {
    matches!(requested, DEFAULT_MODEL | MODEL_ALIAS)
        && matches!(actual, DEFAULT_MODEL | MODEL_ALIAS)
}

fn exact_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<()> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        bail!("ChatGPT subscription response contained an unknown field");
    }
    Ok(())
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

fn sse_data(event: &[u8]) -> Result<String> {
    let event = std::str::from_utf8(event).context("ChatGPT subscription SSE was not UTF-8")?;
    let mut data = String::new();
    for line in event.lines() {
        if line.len() > MAX_SSE_LINE_BYTES {
            bail!("ChatGPT subscription stream line exceeded the size limit");
        }
        if line.is_empty() || line.starts_with(':') {
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
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::LlmProvider;
    use crate::tools::types::ToolInputSchema;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        json!({
            "models":[
                {
                    "slug":"gpt-5.6-sol",
                    "supported_in_api":true,
                    "use_responses_lite":true,
                    "input_modalities":["text","image"],
                    "context_window":1050000
                },
                {
                    "slug":"gpt-5.6",
                    "supported_in_api":true,
                    "use_responses_lite":true,
                    "input_modalities":["text","image"],
                    "context_window":1050000
                }
            ]
        })
        .to_string()
    }

    fn completed_sse(model: &str) -> String {
        format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "type":"response.completed",
                "sequence_number":1,
                "response":{
                    "id":"resp-1",
                    "status":"completed",
                    "model":model,
                    "output":[
                        {"type":"reasoning","summary":[],"encrypted_content":"opaque-1"},
                        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]},
                        {"type":"function_call","call_id":"call-2","name":"read","arguments":"{\"path\":\"README.md\"}"}
                    ],
                    "usage":{"input_tokens":12,"output_tokens":7}
                }
            })
        )
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

    #[test]
    fn canonical_request_preserves_ordered_reasoning_tools_results_and_lite_shape() {
        let request = ProviderRequest::new(vec![
            Message::user("inspect"),
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
        assert_eq!(body["input"][1]["role"], "developer");
        assert_eq!(body["input"][3]["type"], "reasoning");
        assert_eq!(body["input"][4]["type"], "function_call");
        assert_eq!(body["input"][5]["type"], "function_call_output");
        assert_eq!(body["reasoning"]["context"], "all_turns");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert!(body.get("instructions").is_none());
        assert!(body.get("tools").is_none());
        assert!(body.get("previous_response_id").is_none());
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
        }
        assert!(subscription_capabilities("gpt-4o")
            .wire_protocol
            .protocol
            .is_none());
    }

    #[test]
    fn malformed_unknown_and_misordered_terminal_events_fail_closed() {
        let unknown = json!({"type":"response.future","sequence_number":1});
        assert!(parse_event(unknown, DEFAULT_MODEL, None, &HashSet::new()).is_err());
        let malformed = json!({
            "type":"response.output_text.delta",
            "sequence_number":1,
            "item_id":"item-1",
            "output_index":0,
            "content_index":0,
            "unexpected":"secret"
        });
        assert!(parse_event(malformed, DEFAULT_MODEL, None, &HashSet::new()).is_err());
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
        )
        .is_err());
    }

    #[tokio::test]
    async fn validated_dispatch_uses_exact_account_routes_and_preserves_terminal_metadata() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                env!("CARGO_PKG_VERSION").into(),
            ))
            .match_header("authorization", "Bearer subscription-secret")
            .match_header("chatgpt-account-id", "account-1")
            .match_header("originator", "finch")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_header("etag", "account-etag")
            .with_body(catalog_body())
            .create_async()
            .await;
        let expected = responses_lite_request(
            &ProviderRequest::new(vec![Message::user("hello")])
                .with_model(DEFAULT_MODEL)
                .with_tools(vec![tool()]),
            ReasoningEffort::High,
        )
        .unwrap();
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .match_header("authorization", "Bearer subscription-secret")
            .match_header("chatgpt-account-id", "account-1")
            .match_header("originator", "finch")
            .match_header("x-openai-internal-codex-responses-lite", "true")
            .match_body(mockito::Matcher::Json(expected))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_header("x-codex-primary-used-percent", "25")
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
        let response = provider
            .send_message(
                &ProviderRequest::new(vec![Message::user("hello")])
                    .with_model(DEFAULT_MODEL)
                    .with_tools(vec![tool()]),
            )
            .await
            .unwrap();
        assert_eq!(response.model, DEFAULT_MODEL);
        assert_eq!(response.usage.unwrap().output_tokens, 7);
        assert_eq!(response.allowance.unwrap().primary_used_percent, Some(25));
        assert!(matches!(
            response.content.first(),
            Some(ContentBlock::OpaqueReasoning { encrypted_content }) if encrypted_content == "opaque-1"
        ));
        assert!(matches!(
            response.content.last(),
            Some(ContentBlock::ToolUse { id, .. }) if id == "call-2"
        ));
        assert_eq!(source.refreshes.load(Ordering::SeqCst), 0);
        models.assert_async().await;
        inference.assert_async().await;
    }

    #[tokio::test]
    async fn one_pre_stream_unauthorized_refreshes_same_account_once() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/backend-api/codex/models")
            .match_header("authorization", "Bearer subscription-secret")
            .with_status(200)
            .with_body(catalog_body())
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
            .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .unwrap();
        assert_eq!(source.refreshes.load(Ordering::SeqCst), 1);
        first.assert_async().await;
        second.assert_async().await;
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
        let config = crate::config::load_config()?;
        let profile = config
            .providers
            .iter()
            .find(|entry| {
                matches!(
                    entry,
                    crate::config::ProviderEntry::Credentialed {
                        provider: CredentialProvider::ChatgptSubscription,
                        ..
                    }
                )
            })
            .context("No Finch ChatGPT subscription profile is configured")?
            .profile_name();
        let provider = crate::providers::create_provider_profile_from_config(&config, &profile)?;
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
