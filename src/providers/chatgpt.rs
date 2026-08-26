//! Direct ChatGPT subscription Responses transport.
//!
//! This is deliberately separate from [`super::openai::OpenAIProvider`]. A
//! ChatGPT OAuth credential is accepted only by the subscription backend and
//! an OpenAI Platform API key is accepted only by the Platform provider.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::{Client, Response};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use super::chatgpt_auth::{
    ChatGptAccountStatus, ChatGptAuth, PendingDeviceLogin, CHATGPT_PROTOCOL_REVISION,
};
use super::{LlmProvider, ProviderRequest, ProviderResponse, StreamChunk};
use crate::claude::types::ContentBlock;
use crate::tools::types::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const DEFAULT_MODEL: &str = "gpt-5.6-sol";
const MAX_ERROR_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;
const MAX_STREAM_BYTES: usize = 64 * 1024 * 1024;
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Wizard-safe async login task. UI code renders `pending()`, calls `cancel()`
/// from its existing escape/cancel path, and persists a profile only after
/// `finish()` returns conformance-tested models.
pub struct ChatGptSetupFlow {
    auth: ChatGptAuth,
    pending: PendingDeviceLogin,
    cancellation: tokio_util::sync::CancellationToken,
}

#[derive(Debug)]
pub struct ChatGptSetupOutcome {
    pub account: ChatGptAccountStatus,
    pub models: Vec<String>,
    pub preferred_model: String,
}

impl ChatGptSetupFlow {
    pub async fn begin(credential_ref: impl Into<String>) -> Result<Self> {
        Self::begin_with_replacement(credential_ref, false).await
    }

    /// Start an explicitly confirmed replacement of a shared named account.
    pub async fn begin_replacing(credential_ref: impl Into<String>) -> Result<Self> {
        Self::begin_with_replacement(credential_ref, true).await
    }

    async fn begin_with_replacement(
        credential_ref: impl Into<String>,
        replace: bool,
    ) -> Result<Self> {
        let auth = ChatGptAuth::new(credential_ref)?;
        if auth.account_status()?.is_some() && !replace {
            bail!("ChatGPT credential reference already names an account; replacement requires explicit confirmation");
        }
        let pending = auth.begin_device_login().await?;
        Ok(Self {
            auth,
            pending,
            cancellation: tokio_util::sync::CancellationToken::new(),
        })
    }

    pub fn pending(&self) -> &PendingDeviceLogin {
        &self.pending
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Cloneable handle retained by the wizard event loop after `finish(self)`
    /// is moved into its background task.
    pub fn cancellation_handle(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation.clone()
    }

    pub async fn finish(self) -> Result<ChatGptSetupOutcome> {
        let account = self
            .auth
            .finish_device_login(&self.pending, self.cancellation.clone())
            .await?;
        if self.cancellation.is_cancelled() {
            let cleanup = self.auth.logout().await;
            bail!(
                "ChatGPT setup was cancelled after authorization; issued credentials were revoked and tombstoned: {}",
                if cleanup.is_ok() {
                    "yes"
                } else {
                    "cleanup also failed"
                }
            );
        }
        let provider = ChatGptProvider::new(self.auth.credential_ref().as_str().to_string())?;
        let models = match provider.available_models().await {
            Ok(models) => models,
            Err(error) => {
                let cleanup = self.auth.logout().await;
                bail!(
                    "ChatGPT setup transport conformance failed ({error}); issued credentials were revoked and tombstoned: {}",
                    if cleanup.is_ok() { "yes" } else { "cleanup also failed" }
                );
            }
        };
        if self.cancellation.is_cancelled() {
            let cleanup = self.auth.logout().await;
            bail!(
                "ChatGPT setup was cancelled during model validation; issued credentials were revoked and tombstoned: {}",
                if cleanup.is_ok() {
                    "yes"
                } else {
                    "cleanup also failed"
                }
            );
        }
        let preferred_model = match provider.preferred_account_model().await {
            Ok(model) => model,
            Err(error) => {
                let cleanup = self.auth.logout().await;
                bail!(
                    "ChatGPT setup model validation failed ({error}); issued credentials were revoked and tombstoned: {}",
                    if cleanup.is_ok() {
                        "yes"
                    } else {
                        "cleanup also failed"
                    }
                );
            }
        };
        Ok(ChatGptSetupOutcome {
            account,
            models,
            preferred_model,
        })
    }
}

#[derive(Clone)]
pub struct ChatGptProvider {
    client: Client,
    auth: ChatGptAuth,
    base_url: String,
    model: String,
    models: std::sync::Arc<Mutex<Option<ModelCache>>>,
    context_limit: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone)]
struct ModelCache {
    credential_generation: String,
    account_id: String,
    models: Vec<String>,
    fetched_at: tokio::time::Instant,
}

impl Drop for ModelCache {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.credential_generation.zeroize();
        self.account_id.zeroize();
    }
}

const MODEL_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(serde::Deserialize)]
struct ModelsEnvelope {
    models: Vec<AdvertisedModel>,
}

#[derive(serde::Deserialize)]
struct AdvertisedModel {
    slug: String,
    supported_in_api: bool,
    #[serde(default)]
    minimal_client_version: Option<Vec<u64>>,
    #[serde(default)]
    experimental_supported_tools: Vec<String>,
    #[serde(default)]
    context_window: Option<i64>,
    #[serde(default = "default_text_modality")]
    input_modalities: Vec<String>,
}

fn default_text_modality() -> Vec<String> {
    vec!["text".to_string()]
}

impl std::fmt::Debug for ChatGptProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptProvider")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("auth", &"<Finch credential reference>")
            .finish()
    }
}

impl ChatGptProvider {
    pub fn new(credential_ref: impl Into<String>) -> Result<Self> {
        Self::with_options(
            ChatGptAuth::new(credential_ref)?,
            DEFAULT_BASE_URL,
            DEFAULT_MODEL,
        )
    }

    pub fn with_options(
        auth: ChatGptAuth,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        validate_subscription_endpoint(&base_url)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // Streaming has an explicit per-chunk idle timeout below.
            .timeout(Duration::from_secs(10 * 60))
            .build()
            .context("Failed to create ChatGPT provider client")?;
        Ok(Self {
            client,
            auth,
            base_url,
            model: model.into(),
            models: std::sync::Arc::new(Mutex::new(None)),
            context_limit: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(128_000)),
        })
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub async fn available_models(&self) -> Result<Vec<String>> {
        let tokens = self.auth.tokens().await?;
        if let Some(cache) = self.models.lock().await.as_ref() {
            if cache.credential_generation == tokens.generation
                && cache.account_id == tokens.account_id
                && cache.fetched_at.elapsed() <= MODEL_CACHE_TTL
            {
                return Ok(cache.models.clone());
            }
        }
        let models = self.fetch_models(&tokens).await?;
        *self.models.lock().await = Some(ModelCache {
            credential_generation: tokens.generation.clone(),
            account_id: tokens.account_id.clone(),
            models: models.clone(),
            fetched_at: tokio::time::Instant::now(),
        });
        Ok(models)
    }

    async fn fetch_models(
        &self,
        tokens: &super::chatgpt_auth::ChatGptTokens,
    ) -> Result<Vec<String>> {
        let response = self
            .client
            // The private `client_version` field is a Codex compatibility
            // version, not an arbitrary caller version. Finch must not put its
            // own semver there or impersonate an upstream Codex build.
            .get(format!("{}/models", self.base_url))
            .bearer_auth(&tokens.access_token)
            .header("chatgpt-account-id", &tokens.account_id)
            .header("originator", "finch")
            .header("x-finch-chatgpt-protocol", CHATGPT_PROTOCOL_REVISION)
            .send()
            .await
            .context("Failed to discover ChatGPT subscription models")?;
        let status = response.status();
        let body = read_response_bounded(response, 2 * 1024 * 1024).await?;
        if !status.is_success() {
            bail!("ChatGPT model discovery failed (HTTP {status})");
        }
        let value: ModelsEnvelope =
            serde_json::from_slice(&body).context("ChatGPT model discovery contract changed")?;
        if value.models.len() > 512 {
            bail!("ChatGPT model discovery exceeded the model limit");
        }
        self.context_limit
            .store(128_000, std::sync::atomic::Ordering::Relaxed);
        let mut result = Vec::new();
        for model in value.models.into_iter().take(512) {
            let slug = model.slug;
            if slug.is_empty() || slug.len() > 256 || slug.chars().any(char::is_control) {
                bail!("ChatGPT model identifier is invalid");
            }
            if model.minimal_client_version.is_some() {
                bail!("ChatGPT model catalog requires an upstream client compatibility version Finch cannot truthfully claim");
            }
            if model
                .experimental_supported_tools
                .iter()
                .any(|tool| tool.len() > 128 || tool.chars().any(char::is_control))
            {
                bail!("ChatGPT model catalog advertised an invalid tool capability");
            }
            if !model
                .input_modalities
                .iter()
                .any(|modality| modality == "text")
            {
                continue;
            }
            if let Some(context_window) = model.context_window {
                if !(1..=4_000_000).contains(&context_window) {
                    bail!("ChatGPT model catalog advertised an invalid context window");
                }
                if slug == self.model {
                    self.context_limit.store(
                        context_window as usize,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
            }
            if model.supported_in_api {
                result.push(slug);
            }
        }
        if result.is_empty() {
            bail!("ChatGPT account advertised no usable models");
        }
        Ok(result)
    }

    /// Select GPT-5.6 Sol only when the account advertises it.
    pub async fn preferred_account_model(&self) -> Result<String> {
        let models = self.available_models().await?;
        if models.iter().any(|model| model == "gpt-5.6-sol") {
            return Ok("gpt-5.6-sol".to_string());
        }
        bail!("ChatGPT account does not advertise the required gpt-5.6-sol model")
    }

    async fn start_stream(&self, request: &ProviderRequest) -> Result<Response> {
        let selected_model = if request.model.trim().is_empty() {
            self.model.as_str()
        } else {
            request.model.as_str()
        };
        let advertised = self.available_models().await?;
        if !advertised.iter().any(|model| model == selected_model) {
            bail!("ChatGPT account does not advertise configured model `{selected_model}`");
        }
        let mut tokens = self.auth.tokens().await?;
        let body = responses_request(request, &self.model)?;
        let body = serde_json::to_vec(&body).context("Failed to encode ChatGPT request")?;
        if body.len() > MAX_REQUEST_BYTES {
            bail!("ChatGPT subscription request exceeded the size limit");
        }
        let response = loop {
            let response = self
                .client
                .post(format!("{}/responses", self.base_url))
                .bearer_auth(&tokens.access_token)
                .header("chatgpt-account-id", &tokens.account_id)
                .header("accept", "text/event-stream")
                .header("originator", "finch")
                .header("user-agent", concat!("finch/", env!("CARGO_PKG_VERSION")))
                .header("x-finch-chatgpt-protocol", CHATGPT_PROTOCOL_REVISION)
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .await
                .context("Failed to start ChatGPT subscription response")?;
            if response.status() != reqwest::StatusCode::UNAUTHORIZED {
                break response;
            }
            let _ = read_response_bounded(response, MAX_ERROR_BYTES).await?;
            tokens = self
                .auth
                .tokens_after_unauthorized(&tokens.generation)
                .await?;
            let retry = self
                .client
                .post(format!("{}/responses", self.base_url))
                .bearer_auth(&tokens.access_token)
                .header("chatgpt-account-id", &tokens.account_id)
                .header("accept", "text/event-stream")
                .header("originator", "finch")
                .header("user-agent", concat!("finch/", env!("CARGO_PKG_VERSION")))
                .header("x-finch-chatgpt-protocol", CHATGPT_PROTOCOL_REVISION)
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .await
                .context("Failed to retry ChatGPT subscription response after refresh")?;
            break retry;
        };
        if !response.status().is_success() {
            let status = response.status();
            let _ = read_response_bounded(response, MAX_ERROR_BYTES).await?;
            bail!("ChatGPT subscription response failed (HTTP {status})");
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type.starts_with("text/event-stream") {
            let _ = read_response_bounded(response, MAX_ERROR_BYTES).await?;
            bail!("ChatGPT subscription response contract changed (expected event stream)");
        }
        Ok(response)
    }
}

fn validate_subscription_endpoint(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).context("Invalid ChatGPT subscription endpoint")?;
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        bail!("Invalid ChatGPT subscription endpoint");
    }
    let host = url.host_str().context("ChatGPT endpoint omitted host")?;
    let production = url.scheme() == "https" && host.eq_ignore_ascii_case("chatgpt.com");
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !production && !loopback {
        bail!("Refusing to send a ChatGPT subscription token to an untrusted endpoint");
    }
    Ok(())
}

#[async_trait]
impl LlmProvider for ChatGptProvider {
    async fn send_message(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let response = self.start_stream(request).await?;
        let (mut receiver, completion) = spawn_sse(response, advertised_tool_names(request));
        let mut content = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            match chunk? {
                StreamChunk::TextDelta(_) => {}
                StreamChunk::ContentBlockComplete(block) => content.push(block),
                StreamChunk::Usage { .. } => {}
            }
        }
        let completed = completion
            .await
            .context("ChatGPT response task stopped")??;
        let actual_model = completed
            .header_model
            .clone()
            .or_else(|| completed.model.clone())
            .context("ChatGPT response did not identify the actual model used")?;
        Ok(ProviderResponse {
            id: completed.response_id,
            model: actual_model,
            content,
            stop_reason: Some(completed.stop_reason),
            role: "assistant".to_string(),
            provider: "chatgpt_subscription".to_string(),
        })
    }

    async fn send_message_stream(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let response = self.start_stream(request).await?;
        let (receiver, _completion) = spawn_sse(response, advertised_tool_names(request));
        Ok(receiver)
    }

    fn name(&self) -> &str {
        "chatgpt_subscription"
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn context_limit_tokens(&self) -> usize {
        self.context_limit
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

fn responses_request(request: &ProviderRequest, default_model: &str) -> Result<Value> {
    let model = if request.model.trim().is_empty() {
        default_model
    } else {
        request.model.as_str()
    };
    let mut input = Vec::new();
    for message in &request.messages {
        let mut content = Vec::new();
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => content.push(json!({
                    "type": if message.role == "assistant" { "output_text" } else { "input_text" },
                    "text": text,
                })),
                ContentBlock::Image { .. } => {
                    bail!("ChatGPT subscription image mapping is not yet supported")
                }
                ContentBlock::ToolUse {
                    id,
                    name,
                    input: arguments,
                } => {
                    flush_message_content(&mut input, &message.role, &mut content);
                    input.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": serde_json::to_string(arguments)
                            .context("Failed to encode Finch tool request")?,
                    }));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content: output,
                    ..
                } => {
                    flush_message_content(&mut input, &message.role, &mut content);
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": output,
                    }));
                }
            }
        }
        flush_message_content(&mut input, &message.role, &mut content);
    }
    let tools = request.tools.as_deref().map(map_tools).transpose()?;
    if input.is_empty() {
        bail!("ChatGPT subscription request has no input history");
    }
    let mut body = json!({
        "model": model,
        "instructions": request.system.clone().unwrap_or_default(),
        "input": input,
        "store": false,
        "stream": true
    });
    if let Some(tools) = tools {
        let object = body.as_object_mut().expect("JSON object");
        object.insert("tools".to_string(), Value::Array(tools));
        object.insert("tool_choice".to_string(), Value::String("auto".to_string()));
        // Finch executes tools sequentially through its own approval lifecycle;
        // do not claim parallel execution support to the private backend.
        object.insert("parallel_tool_calls".to_string(), Value::Bool(false));
    }
    if request.system.as_deref().is_none_or(str::is_empty) {
        body.as_object_mut()
            .expect("JSON object")
            .remove("instructions");
    }
    Ok(body)
}

fn flush_message_content(input: &mut Vec<Value>, role: &str, content: &mut Vec<Value>) {
    if content.is_empty() {
        return;
    }
    input.push(json!({
        "type": "message",
        "role": role,
        "content": std::mem::take(content),
    }));
}

fn map_tools(tools: &[ToolDefinition]) -> Result<Vec<Value>> {
    if tools.len() > 256 {
        bail!("Too many Finch tools for ChatGPT subscription request");
    }
    tools
        .iter()
        .map(|tool| {
            if tool.name.is_empty()
                || tool.name.len() > 128
                || tool.name.chars().any(char::is_control)
            {
                bail!("Finch tool name is invalid");
            }
            Ok(json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": {
                    "type": tool.input_schema.schema_type,
                    "properties": tool.input_schema.properties,
                    "required": tool.input_schema.required,
                    "additionalProperties": false,
                },
                "strict": false,
            }))
        })
        .collect()
}

#[derive(Debug, Default)]
struct Completion {
    response_id: String,
    model: Option<String>,
    header_model: Option<String>,
    stop_reason: String,
    authoritative_text: Option<String>,
    pending_blocks: Vec<ContentBlock>,
    completed: bool,
    allowed_tools: HashSet<String>,
}

fn spawn_sse(
    response: Response,
    allowed_tools: HashSet<String>,
) -> (
    mpsc::Receiver<Result<StreamChunk>>,
    tokio::task::JoinHandle<Result<Completion>>,
) {
    let (sender, receiver) = mpsc::channel(64);
    let handle = tokio::spawn(async move {
        let failure_sender = sender.clone();
        let result: Result<Completion> = async move {
            let header_model = response
                .headers()
                .get("openai-model")
                .or_else(|| response.headers().get("x-openai-model"))
                .map(|value| {
                    value
                        .to_str()
                        .context("ChatGPT actual-model header was invalid")
                })
                .transpose()?
                .map(str::to_string);
            if header_model.as_ref().is_some_and(|model| {
                model.is_empty() || model.len() > 256 || model.chars().any(char::is_control)
            }) {
                bail!("ChatGPT actual-model header exceeded protocol limits");
            }
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut total = 0usize;
            let mut completion = Completion {
                header_model,
                allowed_tools,
                ..Completion::default()
            };
            loop {
                let next = tokio::select! {
                    _ = sender.closed() => return Ok(completion),
                    next = tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()) => {
                        next.context("ChatGPT subscription stream timed out")?
                    }
                };
                let Some(chunk) = next else { break };
                let chunk = chunk.context("ChatGPT subscription stream failed")?;
                total = total.saturating_add(chunk.len());
                if total > MAX_STREAM_BYTES {
                    bail!("ChatGPT subscription stream exceeded the size limit");
                }
                buffer.extend_from_slice(&chunk);
                if buffer.len() > MAX_SSE_EVENT_BYTES && find_event_end(&buffer).is_none() {
                    bail!("ChatGPT subscription stream event exceeded the size limit");
                }
                while let Some((end, separator_len)) = find_event_end(&buffer) {
                    if end > MAX_SSE_EVENT_BYTES {
                        bail!("ChatGPT subscription stream event exceeded the size limit");
                    }
                    let event = buffer.drain(..end).collect::<Vec<_>>();
                    buffer.drain(..separator_len);
                    if let Some(chunk) = parse_sse_event(&event, &mut completion)? {
                        if sender.send(Ok(chunk)).await.is_err() {
                            return Ok(completion);
                        }
                    }
                    if completion.completed {
                        if !buffer.iter().all(u8::is_ascii_whitespace) {
                            bail!("ChatGPT subscription sent data after response.completed");
                        }
                        for block in completion.pending_blocks.iter().cloned() {
                            if sender
                                .send(Ok(StreamChunk::ContentBlockComplete(block)))
                                .await
                                .is_err()
                            {
                                return Ok(completion);
                            }
                        }
                        return Ok(completion);
                    }
                }
            }
            if completion.response_id.is_empty() {
                bail!("ChatGPT subscription stream ended before response.completed");
            }
            Ok(completion)
        }
        .await;
        if let Err(error) = &result {
            let _ = failure_sender
                .send(Err(anyhow::anyhow!(error.to_string())))
                .await;
        }
        result
    });
    (receiver, handle)
}

fn find_event_end(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2));
    let crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn parse_sse_event(event: &[u8], completion: &mut Completion) -> Result<Option<StreamChunk>> {
    let text = std::str::from_utf8(event).context("ChatGPT stream was not UTF-8")?;
    let mut data = String::new();
    for line in text.lines() {
        if line.len() > MAX_SSE_EVENT_BYTES {
            bail!("ChatGPT subscription stream line exceeded the size limit");
        }
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    let value: Value =
        serde_json::from_str(&data).context("ChatGPT subscription stream contract changed")?;
    match value.get("type").and_then(Value::as_str).unwrap_or_default() {
        "response.output_text.delta" => {
            let _delta = value
                .get("delta")
                .and_then(Value::as_str)
                .context("ChatGPT text delta omitted delta")?;
            // #46 owns the shared provisional/commit lifecycle. Until that
            // lands, direct ChatGPT transport buffers all text and emits only
            // the authoritative terminal output below.
            Ok(None)
        }
        "response.output_item.done" => {
            let item = value
                .get("item")
                .context("ChatGPT output item omitted item")?;
            let item_type = item.get("type").and_then(Value::as_str);
            if item_type == Some("custom_tool_call") {
                bail!("ChatGPT requested an unnegotiated custom tool type");
            }
            if item_type != Some("function_call") {
                return Ok(None);
            }
            let id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .context("ChatGPT tool request omitted call id")?;
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .context("ChatGPT tool request omitted name")?;
            let arguments = item
                .get("arguments")
                .or_else(|| item.get("input"))
                .and_then(Value::as_str)
                .context("ChatGPT tool request omitted arguments")?;
            if id.is_empty()
                || id.len() > 256
                || id.chars().any(char::is_control)
                || name.is_empty()
                || name.len() > 128
                || name.chars().any(char::is_control)
                || arguments.len() > MAX_SSE_EVENT_BYTES
            {
                bail!("ChatGPT tool request exceeded protocol limits");
            }
            if !completion.allowed_tools.contains(name) {
                bail!("ChatGPT requested a function Finch did not advertise");
            }
            let input: Value = serde_json::from_str(arguments)
                .context("ChatGPT tool request contained invalid JSON arguments")?;
            if !input.is_object() {
                bail!("ChatGPT tool request arguments were not an object");
            }
            if completion.pending_blocks.iter().any(|block| {
                matches!(block, ContentBlock::ToolUse { id: existing, .. } if existing == id)
            }) {
                bail!("ChatGPT subscription repeated a tool call identifier");
            }
            completion.pending_blocks.push(ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
            });
            Ok(None)
        }
        "response.completed" => {
            let response = value
                .get("response")
                .context("ChatGPT completion omitted response")?;
            completion.response_id = response
                .get("id")
                .and_then(Value::as_str)
                .context("ChatGPT completion omitted response id")?
                .to_string();
            completion.model = response
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string);
            if completion.response_id.is_empty()
                || completion.response_id.len() > 256
                || completion
                    .model
                    .as_ref()
                    .is_some_and(|model| {
                        model.is_empty()
                            || model.len() > 256
                            || model.chars().any(char::is_control)
                    })
            {
                bail!("ChatGPT completion identifiers exceeded protocol limits");
            }
            completion.authoritative_text = authoritative_response_text(response)?;
            if let Some(text) = completion.authoritative_text.as_ref() {
                completion
                    .pending_blocks
                    .insert(0, ContentBlock::Text { text: text.clone() });
            }
            completion.stop_reason = "end_turn".to_string();
            completion.completed = true;
            Ok(None)
        }
        "response.failed" | "response.incomplete" => {
            bail!("ChatGPT subscription response failed")
        }
        "response.created"
        | "response.in_progress"
        | "response.output_item.added"
        | "response.content_part.added"
        | "response.content_part.done"
        | "response.output_text.done"
        | "response.function_call_arguments.delta"
        | "response.function_call_arguments.done"
        | "response.custom_tool_call_input.delta"
        | "response.custom_tool_call_input.done"
        | "response.reasoning_summary_part.added"
        | "response.reasoning_summary_part.done"
        | "response.reasoning_summary_text.delta"
        | "response.reasoning_summary_text.done"
        | "response.reasoning_text.delta"
        | "response.metadata"
        | "codex.response.metadata"
        | "responsesapi.websocket_timing" => Ok(None),
        kind => bail!("Unknown ChatGPT subscription stream event `{}`; protocol revision {} is no longer compatible", kind.chars().take(128).collect::<String>(), CHATGPT_PROTOCOL_REVISION),
    }
}

fn advertised_tool_names(request: &ProviderRequest) -> HashSet<String> {
    request
        .tools
        .as_ref()
        .into_iter()
        .flatten()
        .map(|tool| tool.name.clone())
        .collect()
}

fn authoritative_response_text(response: &Value) -> Result<Option<String>> {
    let mut parts = Vec::new();
    let Some(output) = response.get("output") else {
        return Ok(None);
    };
    let output = output
        .as_array()
        .context("ChatGPT terminal output was not an array")?;
    if output.len() > 1024 {
        bail!("ChatGPT terminal output exceeded the item limit");
    }
    let mut total = 0usize;
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let content = item
            .get("content")
            .and_then(Value::as_array)
            .context("ChatGPT terminal message omitted content")?;
        if content.len() > 1024 {
            bail!("ChatGPT terminal message exceeded the content limit");
        }
        for content in content {
            if matches!(
                content.get("type").and_then(Value::as_str),
                Some("output_text")
            ) {
                let text = content
                    .get("text")
                    .and_then(Value::as_str)
                    .context("ChatGPT terminal output_text omitted text")?;
                total = total.saturating_add(text.len());
                if total > MAX_STREAM_BYTES {
                    bail!("ChatGPT terminal text exceeded the size limit");
                }
                parts.push(text);
            }
        }
    }
    Ok((!parts.is_empty()).then(|| parts.concat()))
}

async fn read_response_bounded(response: Response, maximum: usize) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Failed to read ChatGPT response")?;
        if bytes.len().saturating_add(chunk.len()) > maximum {
            bail!("ChatGPT response exceeded the size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::Message;
    use crate::providers::chatgpt_auth::{CredentialRef, CredentialStore, FileCredentialStore};
    use std::sync::Arc;

    #[test]
    fn test_response_request_maps_tool_lifecycle_without_executing() {
        let request = ProviderRequest::new(vec![
            Message::user("read it"),
            Message::with_content(
                "assistant",
                vec![ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "read".into(),
                    input: json!({"path": "README.md"}),
                }],
            ),
            Message::with_content(
                "user",
                vec![ContentBlock::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: "contents".into(),
                    is_error: None,
                }],
            ),
        ]);
        let body = responses_request(&request, DEFAULT_MODEL).unwrap();
        assert_eq!(body["input"][1]["type"], "function_call");
        assert_eq!(body["input"][2]["type"], "function_call_output");
        assert_eq!(body["store"], false);
    }

    #[test]
    fn test_response_request_preserves_mixed_block_order() {
        let request = ProviderRequest::new(vec![Message::with_content(
            "assistant",
            vec![
                ContentBlock::Text {
                    text: "before".into(),
                },
                ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "read".into(),
                    input: json!({"path": "README.md"}),
                },
                ContentBlock::Text {
                    text: "after".into(),
                },
            ],
        )]);
        let body = responses_request(&request, DEFAULT_MODEL).unwrap();
        assert_eq!(body["input"][0]["content"][0]["text"], "before");
        assert_eq!(body["input"][1]["type"], "function_call");
        assert_eq!(body["input"][2]["content"][0]["text"], "after");
    }

    #[test]
    fn test_sse_text_and_tool_calls_map_to_finch_content_blocks() {
        let mut completion = Completion {
            allowed_tools: HashSet::from(["read".to_string()]),
            ..Completion::default()
        };
        let text = br#"data: {"type":"response.output_text.delta","delta":"hello"}"#;
        assert!(parse_sse_event(text, &mut completion).unwrap().is_none());
        let tool = br#"data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call-1","name":"read","arguments":"{\"path\":\"README.md\"}"}}"#;
        assert!(parse_sse_event(tool, &mut completion).unwrap().is_none());
        assert!(matches!(
            &completion.pending_blocks[0],
            ContentBlock::ToolUse { id, name, .. } if id == "call-1" && name == "read"
        ));
    }

    #[test]
    fn test_sse_malformed_tool_arguments_fail_closed() {
        let mut completion = Completion {
            allowed_tools: HashSet::from(["read".to_string()]),
            ..Completion::default()
        };
        let tool = br#"data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call-1","name":"read","arguments":"{"}}"#;
        assert!(parse_sse_event(tool, &mut completion).is_err());
        let custom = br#"data: {"type":"response.output_item.done","item":{"type":"custom_tool_call","call_id":"call-2","name":"shell","input":"{}"}}"#;
        assert!(parse_sse_event(custom, &mut completion).is_err());
    }

    #[test]
    fn test_model_catalog_requires_explicit_api_support_and_no_codex_minimum() {
        let missing_support = serde_json::from_value::<ModelsEnvelope>(json!({
            "models": [{"slug": "gpt-test"}]
        }));
        assert!(missing_support.is_err());

        let constrained = serde_json::from_value::<ModelsEnvelope>(json!({
            "models": [{
                "slug": "gpt-test",
                "supported_in_api": true,
                "minimal_client_version": [1, 2, 3]
            }]
        }))
        .unwrap();
        assert!(constrained.models[0].minimal_client_version.is_some());
    }

    #[test]
    fn test_sse_uses_earliest_delimiter_and_rejects_oversized_terminated_event() {
        assert_eq!(find_event_end(b"a\r\n\r\nb\n\n"), Some((1, 4)));
        let oversized = format!("data: {{\"type\":\"{}\"}}", "x".repeat(MAX_SSE_EVENT_BYTES));
        let mut completion = Completion::default();
        assert!(parse_sse_event(oversized.as_bytes(), &mut completion).is_err());
    }

    #[tokio::test]
    async fn test_stream_contract_error_is_delivered_to_stream_consumer() {
        let mut server = mockito::Server::new_async().await;
        let _stream = server
            .mock("GET", "/stream")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("data: {\"type\":\"response.future_event\"}\n\n")
            .create_async()
            .await;
        let response = reqwest::Client::new()
            .get(format!("{}/stream", server.url()))
            .send()
            .await
            .unwrap();
        let (mut receiver, completion) = spawn_sse(response, HashSet::new());
        let error = receiver
            .recv()
            .await
            .expect("stream must report its terminal error")
            .unwrap_err()
            .to_string();
        assert!(error.contains("protocol revision"));
        assert!(completion.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn test_stream_rejects_replay_after_terminal_before_releasing_tools() {
        let mut server = mockito::Server::new_async().await;
        let _stream = server
            .mock("GET", "/stream")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(concat!(
                "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"read\",\"arguments\":\"{}\"}}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"model\":\"gpt-test\"}}\n\n",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"late\"}\n\n"
            ))
            .create_async()
            .await;
        let response = reqwest::Client::new()
            .get(format!("{}/stream", server.url()))
            .send()
            .await
            .unwrap();
        let (mut receiver, completion) = spawn_sse(response, HashSet::from(["read".to_string()]));
        let first = receiver.recv().await.unwrap().unwrap_err().to_string();
        assert!(first.contains("after response.completed"));
        assert!(receiver.recv().await.is_none());
        assert!(completion.await.unwrap().is_err());
    }

    #[test]
    fn test_subscription_token_cannot_be_routed_to_platform_api() {
        let dir = tempfile::tempdir().unwrap();
        let auth = ChatGptAuth::with_options(
            "http://127.0.0.1:1",
            "client",
            CredentialRef::parse("chatgpt:boundary").unwrap(),
            Arc::new(FileCredentialStore::new(dir.path().join("credentials"))),
        )
        .unwrap();
        let error = ChatGptProvider::with_options(auth, "https://api.openai.com/v1", "gpt-5.6-sol")
            .unwrap_err()
            .to_string();
        assert!(error.contains("untrusted endpoint"));
    }

    #[tokio::test]
    async fn test_factory_transport_boundary_uses_subscription_headers_and_streams_text() {
        let mut server = mockito::Server::new_async().await;
        let _models = server
            .mock("GET", "/backend-api/codex/models")
            .match_header("authorization", "Bearer access-token")
            .match_header("chatgpt-account-id", "account-test")
            .with_status(200)
            .with_body(
                json!({
                    "models": [{"slug": "gpt-5.6-sol", "supported_in_api": true}]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let _response = server
            .mock("POST", "/backend-api/codex/responses")
            .match_header("authorization", "Bearer access-token")
            .match_header("chatgpt-account-id", "account-test")
            .match_header("originator", "finch")
            .match_body(mockito::Matcher::PartialJson(json!({
                "model": "gpt-5.6-sol",
                "store": false,
                "stream": true
            })))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", "gpt-5.6-sol-safety-routed")
            .with_body(concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"model\":\"gpt-5.6-sol\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello\"}]}]}}\n\n"
            ))
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileCredentialStore::new(dir.path().join("credentials")));
        let reference = CredentialRef::parse("chatgpt:personal").unwrap();
        let record = json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "id_token": null,
            "account_id": "account-test",
            "identity": {
                "authorization_endpoint": server.url(),
                "client_id": "client",
                "subscription_endpoint": "https://chatgpt.com/backend-api/codex",
                "observed_subject": "subject-test",
                "observed_issuer": null,
                "observed_audiences": [],
                "observed_scopes": []
            },
            "expires_at": u64::MAX,
            "generation": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "tombstone": false
        });
        store
            .compare_and_swap(&reference, None, &serde_json::to_vec(&record).unwrap())
            .unwrap();
        let auth = ChatGptAuth::with_options(server.url(), "client", reference, store).unwrap();
        let provider = ChatGptProvider::with_options(
            auth,
            format!("{}/backend-api/codex", server.url()),
            "gpt-5.6-sol",
        )
        .unwrap();
        let response = provider
            .send_message(&ProviderRequest::new(vec![Message::user("hi")]))
            .await
            .unwrap();
        assert_eq!(response.provider, "chatgpt_subscription");
        assert_eq!(response.model, "gpt-5.6-sol-safety-routed");
        assert_eq!(response.text(), "hello");
    }

    #[tokio::test]
    #[ignore = "requires explicit FINCH_CHATGPT_LIVE=1 and independent security authorization"]
    async fn test_live_chatgpt_subscription_smoke_opt_in() -> Result<()> {
        if std::env::var("FINCH_CHATGPT_LIVE").as_deref() != Ok("1") {
            bail!("Set FINCH_CHATGPT_LIVE=1 only after independent security authorization");
        }
        let credential_ref = std::env::var("FINCH_CHATGPT_CREDENTIAL_REF")
            .context("FINCH_CHATGPT_CREDENTIAL_REF must name a Finch credential record")?;
        let provider = ChatGptProvider::new(credential_ref)?;
        let model = provider.preferred_account_model().await?;
        let response = provider
            .with_model(model.clone())
            .send_message(&ProviderRequest::new(vec![Message::user(
                "Reply with the single word finch.",
            )]))
            .await?;
        if response.provider != "chatgpt_subscription" || response.model != model {
            bail!("Live ChatGPT subscription response provenance was inconsistent");
        }
        if response.text().trim().is_empty() {
            bail!("Live ChatGPT subscription response was empty");
        }
        Ok(())
    }
}
