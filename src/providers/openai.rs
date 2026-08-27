// OpenAI API provider implementation
//
// This provider works for both OpenAI (GPT-4, etc.) and Grok (X.AI)
// since they use compatible API formats.

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine;
use futures::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;

use super::endpoints::ProviderEndpoints;
use super::types::{
    CapabilitySupport, ModelCapabilities, ModelFeature, ProviderRequest, ProviderResponse,
    StreamChunk, WireProtocol,
};
use super::{LlmProvider, ProviderBackend, ReasoningCapability, ValidatedProviderRequest};
use crate::claude::retry::{with_retry, NonRetriableError};
use crate::claude::types::{ContentBlock, ImageSource};
use crate::config::ReasoningEffort;

const REQUEST_TIMEOUT_SECS: u64 = 60;
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
const MAX_SSE_TOTAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportRule {
    /// Current official OpenAI Chat Completions contract for GPT-5.6.
    CanonicalGpt56ChatCompletions,
    /// Historical OpenAI-compatible shape used by xAI, Groq, Mistral, Ollama,
    /// remote Finch, custom endpoints, and pre-GPT-5.6 OpenAI models.
    CompatibleChatCompletions,
}

/// Parse an API error body and return a human-friendly message with hints.
///
/// Most providers return `{"error": {"message": "...", "type": "...", "code": "..."}}`.
fn friendly_api_error(status: reqwest::StatusCode, body: &str) -> String {
    // Try to extract the inner message from standard JSON error format
    let extracted = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        });

    let msg = extracted.as_deref().unwrap_or(body.trim());

    // Provide actionable hints based on status code
    let hint = match status.as_u16() {
        401 => " — Check that your API key is correct in ~/.finch/config.toml",
        403 => " — Your API key may lack permissions for this model",
        429 => " — You've hit a rate limit; wait a moment before retrying",
        400 => " — The request was malformed (this may be a finch bug; please report it)",
        404 => " — Model not found; check the model name in your config",
        500 | 502 | 503 => " — The provider is having issues; try again in a moment",
        _ => "",
    };

    format!("API error {}{}: {}", status, hint, msg)
}

fn validate_image_source(source: &ImageSource) -> Result<OpenAIImageUrl> {
    if source.source_type != "base64" {
        anyhow::bail!("OpenAI images must use a base64 source");
    }
    let expected_magic: &[u8] = match source.media_type.as_str() {
        "image/png" => b"\x89PNG\r\n\x1a\n",
        "image/jpeg" => b"\xff\xd8\xff",
        other => anyhow::bail!(
            "OpenAI image media type '{}' is unsupported; expected image/png or image/jpeg",
            other
        ),
    };
    let estimated_decoded = source.data.len().saturating_mul(3) / 4;
    if estimated_decoded > MAX_IMAGE_BYTES {
        anyhow::bail!("OpenAI image exceeded the 10 MiB decoded-size limit");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&source.data)
        .context("OpenAI image contained invalid base64")?;
    if bytes.len() > MAX_IMAGE_BYTES {
        anyhow::bail!("OpenAI image exceeded the 10 MiB decoded-size limit");
    }
    if !bytes.starts_with(expected_magic) {
        anyhow::bail!("OpenAI image bytes did not match the declared media type");
    }
    Ok(OpenAIImageUrl {
        url: format!("data:{};base64,{}", source.media_type, source.data),
    })
}

async fn read_api_error(
    response: reqwest::Response,
    status: reqwest::StatusCode,
    rule: TransportRule,
) -> String {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(next) = stream.next().await {
        let Ok(bytes) = next else { break };
        let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(body.len());
        if remaining == 0 {
            break;
        }
        body.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
    }
    if rule == TransportRule::CanonicalGpt56ChatCompletions {
        // Upstream error bodies can reflect prompts or tool arguments. They are
        // deliberately consumed with a bound but never surfaced or logged.
        return friendly_api_error(status, "response body redacted");
    }
    friendly_api_error(status, &String::from_utf8_lossy(&body))
}

async fn read_bounded_response_body(response: reqwest::Response) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(next) = stream.next().await {
        let bytes = next.context("Failed to read OpenAI response body")?;
        if body.len().saturating_add(bytes.len()) > MAX_RESPONSE_BYTES {
            anyhow::bail!("OpenAI response exceeded the 32 MiB payload limit");
        }
        body.extend_from_slice(&bytes);
    }
    Ok(body)
}

fn validate_canonical_response_shape(value: &serde_json::Value) -> Result<()> {
    let root = value
        .as_object()
        .context("OpenAI response was not a JSON object")?;
    reject_unknown_keys(
        root,
        &[
            "id",
            "object",
            "created",
            "model",
            "choices",
            "usage",
            "service_tier",
            "system_fingerprint",
        ],
        "response",
    )?;
    let choices = root
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .context("OpenAI response omitted a valid choices array")?;
    for choice in choices {
        let choice = choice
            .as_object()
            .context("OpenAI response choice was not an object")?;
        reject_unknown_keys(
            choice,
            &["index", "message", "finish_reason", "logprobs"],
            "response choice",
        )?;
        let message = choice
            .get("message")
            .and_then(serde_json::Value::as_object)
            .context("OpenAI response choice omitted a valid message")?;
        reject_unknown_keys(
            message,
            &["role", "content", "tool_calls", "refusal", "annotations"],
            "response message",
        )?;
        if message.get("refusal").is_some_and(|value| !value.is_null()) {
            anyhow::bail!("OpenAI response contained an unsupported refusal item");
        }
        if message
            .get("annotations")
            .is_some_and(|value| value.as_array().is_none_or(|items| !items.is_empty()))
        {
            anyhow::bail!("OpenAI response contained unsupported annotations");
        }
        if let Some(tool_calls) = message.get("tool_calls") {
            let tool_calls = tool_calls
                .as_array()
                .context("OpenAI response tool_calls was not an array")?;
            for tool_call in tool_calls {
                let tool_call = tool_call
                    .as_object()
                    .context("OpenAI response tool call was not an object")?;
                reject_unknown_keys(tool_call, &["id", "type", "function"], "tool call")?;
                let function = tool_call
                    .get("function")
                    .and_then(serde_json::Value::as_object)
                    .context("OpenAI response tool call omitted a function object")?;
                reject_unknown_keys(function, &["name", "arguments"], "tool function")?;
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct CanonicalStreamState {
    response_id: Option<String>,
    model: Option<String>,
    terminal_reason: Option<String>,
    done: bool,
    accumulated_text: String,
    tool_calls: Vec<(String, String, String)>,
}

fn reject_unknown_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    context: &str,
) -> Result<()> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        anyhow::bail!(
            "OpenAI stream contained unknown {} field '{}'",
            context,
            key
        );
    }
    Ok(())
}

fn validate_canonical_chunk_shape(value: &serde_json::Value) -> Result<()> {
    let root = value
        .as_object()
        .context("OpenAI stream event was not a JSON object")?;
    reject_unknown_keys(
        root,
        &[
            "id",
            "object",
            "created",
            "model",
            "system_fingerprint",
            "service_tier",
            "choices",
            "usage",
        ],
        "event",
    )?;
    let choices = root
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .context("OpenAI stream event omitted a valid choices array")?;
    for choice in choices {
        let choice = choice
            .as_object()
            .context("OpenAI stream choice was not an object")?;
        reject_unknown_keys(
            choice,
            &["index", "delta", "finish_reason", "logprobs"],
            "choice",
        )?;
        let delta = choice
            .get("delta")
            .and_then(serde_json::Value::as_object)
            .context("OpenAI stream choice omitted a valid delta object")?;
        reject_unknown_keys(delta, &["role", "content", "tool_calls"], "delta")?;
        if let Some(tool_calls) = delta.get("tool_calls") {
            let tool_calls = tool_calls
                .as_array()
                .context("OpenAI stream tool_calls was not an array")?;
            for tool_call in tool_calls {
                let tool_call = tool_call
                    .as_object()
                    .context("OpenAI stream tool-call item was not an object")?;
                reject_unknown_keys(
                    tool_call,
                    &["index", "id", "type", "function"],
                    "tool-call item",
                )?;
                if let Some(function) = tool_call.get("function") {
                    let function = function
                        .as_object()
                        .context("OpenAI stream function delta was not an object")?;
                    reject_unknown_keys(function, &["name", "arguments"], "function delta")?;
                }
            }
        }
    }
    Ok(())
}

fn canonical_stream_data(state: &mut CanonicalStreamState, data: &str) -> Result<Vec<StreamChunk>> {
    if state.done {
        anyhow::bail!("OpenAI stream sent data after its terminal marker");
    }
    let value: serde_json::Value =
        serde_json::from_str(data).context("OpenAI stream contained malformed JSON")?;
    validate_canonical_chunk_shape(&value)?;
    let chunk: OpenAIStreamChunk = serde_json::from_value(value)
        .context("OpenAI stream event did not match the documented schema")?;
    if chunk.object.as_deref() != Some("chat.completion.chunk") {
        anyhow::bail!("OpenAI stream contained an unknown event object");
    }
    if let Some(id) = &state.response_id {
        if id != &chunk.id {
            anyhow::bail!("OpenAI stream changed response ID mid-stream");
        }
    } else {
        state.response_id = Some(chunk.id.clone());
    }
    if let Some(model) = &state.model {
        if model != &chunk.model {
            anyhow::bail!("OpenAI stream changed actual model mid-stream");
        }
    } else {
        if chunk.model.trim().is_empty() {
            anyhow::bail!("OpenAI stream omitted the actual model");
        }
        state.model = Some(chunk.model.clone());
    }

    let mut output = Vec::new();
    if let Some(usage) = chunk.usage {
        output.push(StreamChunk::Usage {
            input_tokens: usage.prompt_tokens,
        });
    }
    if chunk.choices.is_empty() {
        if output.is_empty() {
            anyhow::bail!("OpenAI stream chunk had neither a choice nor usage");
        }
        return Ok(output);
    }
    if chunk.choices.len() != 1 || chunk.choices[0].index != 0 {
        anyhow::bail!("OpenAI stream returned an unexpected choice set");
    }
    let choice = &chunk.choices[0];
    if let Some(role) = &choice.delta.role {
        if role != "assistant" {
            anyhow::bail!("OpenAI stream returned unknown delta role '{}'", role);
        }
    }
    if choice.delta.role.is_none()
        && choice.delta.content.is_none()
        && choice.delta.tool_calls.is_none()
        && choice.finish_reason.is_none()
    {
        anyhow::bail!("OpenAI stream returned an empty non-terminal delta");
    }
    if state.terminal_reason.is_some()
        && (choice.delta.content.is_some() || choice.delta.tool_calls.is_some())
    {
        anyhow::bail!("OpenAI stream sent content after terminal status");
    }
    if let Some(content) = &choice.delta.content {
        state.accumulated_text.push_str(content);
        output.push(StreamChunk::TextDelta(content.clone()));
    }
    if let Some(tool_deltas) = &choice.delta.tool_calls {
        for delta in tool_deltas {
            let index = delta
                .index
                .context("OpenAI function-call delta omitted its index")?;
            if index > state.tool_calls.len() {
                anyhow::bail!("OpenAI function-call indices were not contiguous");
            }
            if index == state.tool_calls.len() {
                state
                    .tool_calls
                    .push((String::new(), String::new(), String::new()));
            }
            let call = &mut state.tool_calls[index];
            if let Some(kind) = &delta.tool_type {
                if kind != "function" {
                    anyhow::bail!("OpenAI stream contained an unknown tool-call type");
                }
            }
            if let Some(id) = &delta.id {
                if !call.0.is_empty() && call.0 != *id {
                    anyhow::bail!("OpenAI stream changed a function-call ID");
                }
                call.0 = id.clone();
            }
            if let Some(function) = &delta.function {
                if let Some(name) = &function.name {
                    if !call.1.is_empty() && call.1 != *name {
                        anyhow::bail!("OpenAI stream changed a function-call name");
                    }
                    call.1 = name.clone();
                }
                if let Some(arguments) = &function.arguments {
                    if call.2.len().saturating_add(arguments.len()) > MAX_TOOL_ARGUMENT_BYTES {
                        anyhow::bail!("OpenAI function arguments exceeded the 1 MiB limit");
                    }
                    call.2.push_str(arguments);
                }
            }
        }
    }
    if let Some(reason) = &choice.finish_reason {
        if state.terminal_reason.replace(reason.clone()).is_some() {
            anyhow::bail!("OpenAI stream sent duplicate terminal status");
        }
        if !matches!(
            reason.as_str(),
            "stop" | "length" | "tool_calls" | "content_filter"
        ) {
            anyhow::bail!(
                "OpenAI stream returned unknown terminal status '{}'",
                reason
            );
        }
    }
    Ok(output)
}

fn mark_canonical_done(state: &mut CanonicalStreamState) -> Result<()> {
    if state.done {
        anyhow::bail!("OpenAI stream sent duplicate terminal marker");
    }
    if state.terminal_reason.is_none() {
        anyhow::bail!("OpenAI stream ended before terminal status");
    }
    state.done = true;
    Ok(())
}

async fn publish_canonical_completion(
    state: &CanonicalStreamState,
    tx: &mpsc::Sender<Result<StreamChunk>>,
) -> Result<()> {
    if !state.accumulated_text.is_empty() {
        tx.send(Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text {
            text: state.accumulated_text.clone(),
        })))
        .await
        .map_err(|_| anyhow::anyhow!("OpenAI stream receiver was dropped"))?;
    }
    for block in finalize_tool_calls(&state.tool_calls, true)? {
        tx.send(Ok(StreamChunk::ContentBlockComplete(block)))
            .await
            .map_err(|_| anyhow::anyhow!("OpenAI stream receiver was dropped"))?;
    }
    Ok(())
}

fn sse_line_prefix_exceeds_limit(buffer: &[u8]) -> bool {
    match buffer.iter().position(|byte| *byte == b'\n') {
        Some(position) => position.saturating_add(1) > MAX_SSE_LINE_BYTES,
        None => buffer.len() > MAX_SSE_LINE_BYTES,
    }
}

fn spawn_canonical_stream_parser(
    response: reqwest::Response,
) -> mpsc::Receiver<Result<StreamChunk>> {
    let (tx, rx) = mpsc::channel(100);
    tokio::spawn(async move {
        let mut stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut total = 0usize;
        let mut state = CanonicalStreamState::default();
        loop {
            let next = tokio::select! {
                biased;
                _ = tx.closed() => return,
                next = stream.next() => next,
            };
            let Some(next) = next else {
                if buffer.iter().any(|byte| !byte.is_ascii_whitespace()) {
                    let message = if state.done {
                        "OpenAI stream sent data after its terminal marker"
                    } else {
                        "OpenAI stream reached EOF with an incomplete SSE event"
                    };
                    let _ = tx.send(Err(anyhow::anyhow!(message))).await;
                    return;
                }
                if !state.done {
                    let _ = tx
                        .send(Err(anyhow::anyhow!(
                            "OpenAI stream reached EOF before [DONE]"
                        )))
                        .await;
                    return;
                }
                if let Err(error) = publish_canonical_completion(&state, &tx).await {
                    if !tx.is_closed() {
                        let _ = tx.send(Err(error)).await;
                    }
                }
                return;
            };
            let bytes = match next {
                Ok(bytes) => bytes,
                Err(error) => {
                    let _ = tx.send(Err(error.into())).await;
                    return;
                }
            };
            total = total.saturating_add(bytes.len());
            if total > MAX_SSE_TOTAL_BYTES {
                let _ = tx
                    .send(Err(anyhow::anyhow!(
                        "OpenAI stream exceeded the 4 MiB total limit"
                    )))
                    .await;
                return;
            }
            buffer.extend_from_slice(&bytes);
            if sse_line_prefix_exceeds_limit(&buffer) {
                let _ = tx
                    .send(Err(anyhow::anyhow!(
                        "OpenAI SSE line exceeded the 1 MiB limit"
                    )))
                    .await;
                return;
            }
            while let Some(pos) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=pos).collect::<Vec<_>>();
                if line.len() > MAX_SSE_LINE_BYTES {
                    let _ = tx
                        .send(Err(anyhow::anyhow!(
                            "OpenAI SSE line exceeded the 1 MiB limit"
                        )))
                        .await;
                    return;
                }
                let line = match std::str::from_utf8(&line) {
                    Ok(line) => line.trim_end_matches(&['\r', '\n'][..]),
                    Err(_) => {
                        let _ = tx
                            .send(Err(anyhow::anyhow!("OpenAI SSE was not valid UTF-8")))
                            .await;
                        return;
                    }
                };
                if line.is_empty() {
                    if sse_line_prefix_exceeds_limit(&buffer) {
                        let _ = tx
                            .send(Err(anyhow::anyhow!(
                                "OpenAI SSE line exceeded the 1 MiB limit"
                            )))
                            .await;
                        return;
                    }
                    continue;
                }
                if state.done {
                    let _ = tx
                        .send(Err(anyhow::anyhow!(
                            "OpenAI stream sent data after its terminal marker"
                        )))
                        .await;
                    return;
                }
                if line.starts_with(':') {
                    continue;
                }
                let Some(data) = line.strip_prefix("data:") else {
                    let _ = tx
                        .send(Err(anyhow::anyhow!(
                            "OpenAI stream contained an unknown SSE field"
                        )))
                        .await;
                    return;
                };
                let data = data.strip_prefix(' ').unwrap_or(data);
                if data == "[DONE]" {
                    if let Err(error) = mark_canonical_done(&mut state) {
                        if !tx.is_closed() {
                            let _ = tx.send(Err(error)).await;
                        }
                        return;
                    }
                    if sse_line_prefix_exceeds_limit(&buffer) {
                        let _ = tx
                            .send(Err(anyhow::anyhow!(
                                "OpenAI SSE line exceeded the 1 MiB limit"
                            )))
                            .await;
                        return;
                    }
                    continue;
                }
                match canonical_stream_data(&mut state, data) {
                    Ok(chunks) => {
                        for chunk in chunks {
                            if tx.send(Ok(chunk)).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error)).await;
                        return;
                    }
                }
                if sse_line_prefix_exceeds_limit(&buffer) {
                    let _ = tx
                        .send(Err(anyhow::anyhow!(
                            "OpenAI SSE line exceeded the 1 MiB limit"
                        )))
                        .await;
                    return;
                }
            }
        }
    });
    rx
}

// ─── Streaming tool-call helpers ─────────────────────────────────────────────
//
// OpenAI streams tool calls as *fragments* across multiple SSE deltas.
// We accumulate them into a Vec<(id, name, args_so_far)> and then convert
// them to ContentBlock::ToolUse when the [DONE] marker arrives.

/// Merge one streaming `OpenAIToolCallDelta` into the accumulator.
/// The accumulator is indexed by `delta.index` (default 0).
fn accumulate_tool_call_delta(
    acc: &mut Vec<(String, String, String)>,
    delta: &OpenAIToolCallDelta,
) {
    let idx = delta.index.unwrap_or(0);
    while acc.len() <= idx {
        acc.push((String::new(), String::new(), String::new()));
    }
    if let Some(id) = &delta.id {
        acc[idx].0.push_str(id);
    }
    if let Some(func) = &delta.function {
        if let Some(name) = &func.name {
            acc[idx].1.push_str(name);
        }
        if let Some(args) = &func.arguments {
            acc[idx].2.push_str(args);
        }
    }
}

/// Convert the final accumulator into `ContentBlock::ToolUse` blocks.
///
/// Each entry is `(id, name, json_arguments_string)`.
/// Invalid JSON in the arguments is replaced with an empty object.
fn finalize_tool_calls(
    acc: &[(String, String, String)],
    strict: bool,
) -> Result<Vec<ContentBlock>> {
    acc.iter()
        .filter(|(id, name, _)| !id.is_empty() || !name.is_empty())
        .map(|(id, name, args_str)| {
            if strict && (id.is_empty() || name.is_empty()) {
                anyhow::bail!("OpenAI stream ended with an incomplete function call");
            }
            if strict && args_str.len() > MAX_TOOL_ARGUMENT_BYTES {
                anyhow::bail!("OpenAI function arguments exceeded the 1 MiB limit");
            }
            let input = if strict {
                serde_json::from_str::<serde_json::Value>(args_str)
                    .context("OpenAI returned malformed JSON function arguments")?
            } else {
                serde_json::from_str::<serde_json::Value>(args_str)
                    .unwrap_or_else(|_| serde_json::json!({}))
            };
            if strict && !input.is_object() {
                anyhow::bail!("OpenAI function arguments were not a JSON object");
            }
            Ok(ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input,
            })
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────

/// OpenAI API provider
///
/// Supports both OpenAI and Grok APIs (they use the same format).
#[derive(Clone)]
pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    endpoints: ProviderEndpoints,
    default_model: String,
    provider_name: String,
    reasoning_effort: Option<ReasoningEffort>,
    canonical_openai_endpoint: bool,
}

impl OpenAIProvider {
    /// Create a new OpenAI provider
    pub fn new_openai(api_key: String) -> Result<Self> {
        Self::new(
            api_key,
            "https://api.openai.com".to_string(),
            "/v1/chat/completions",
            "/v1/models",
            "gpt-4o".to_string(),
            "openai".to_string(),
        )
    }

    /// Create a new Grok provider (uses OpenAI-compatible API)
    pub fn new_grok(api_key: String) -> Result<Self> {
        Self::new(
            api_key,
            "https://api.x.ai".to_string(),
            "/v1/chat/completions",
            "/v1/models",
            "grok-4.6".to_string(),
            "grok".to_string(),
        )
    }

    /// Create a new Mistral provider (uses OpenAI-compatible API)
    pub fn new_mistral(api_key: String) -> Result<Self> {
        Self::new(
            api_key,
            "https://api.mistral.ai".to_string(),
            "/v1/chat/completions",
            "/v1/models",
            "mistral-large-2512".to_string(),
            "mistral".to_string(),
        )
    }

    /// Create a new Groq provider (fast inference, uses OpenAI-compatible API)
    /// Note: This is Groq (by Groq Inc), not Grok (by X.AI)
    pub fn new_groq(api_key: String) -> Result<Self> {
        Self::new(
            api_key,
            "https://api.groq.com".to_string(),
            "/openai/v1/chat/completions",
            "/openai/v1/models",
            "openai/gpt-oss-120b".to_string(),
            "groq".to_string(),
        )
    }

    /// Create an Ollama provider using Ollama's OpenAI-compatible API.
    ///
    /// Ollama exposes `/v1/chat/completions` at `base_url` (default: `http://localhost:11434`).
    /// No API key is required — "ollama" is sent as a placeholder.
    pub fn new_ollama(base_url: String, model: String) -> Result<Self> {
        Self::new(
            "ollama".to_string(), // Ollama ignores the Authorization header
            base_url,
            "/v1/chat/completions",
            "/v1/models",
            model,
            "ollama".to_string(),
        )
    }

    /// Create a provider that talks to a remote finch daemon's OpenAI-compatible endpoint.
    ///
    /// The daemon exposes `/v1/chat/completions` at `address`.
    pub fn new_remote_daemon(address: String) -> Result<Self> {
        Self::new(
            String::new(), // no API key for the local/remote daemon
            address,
            "/v1/chat/completions",
            "/v1/models",
            "default".to_string(),
            "remote_daemon".to_string(),
        )
    }

    /// Set custom model for this provider
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Set provider-side reasoning depth for models that support it.
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Create an OpenAI-compatible provider with explicit endpoint paths.
    /// Paths may be relative to `base_url` or complete URLs.
    pub fn new_compatible(
        api_key: String,
        base_url: String,
        chat_path: impl AsRef<str>,
        models_path: impl AsRef<str>,
        default_model: String,
        provider_name: String,
    ) -> Result<Self> {
        Self::new(
            api_key,
            base_url,
            chat_path.as_ref(),
            models_path.as_ref(),
            default_model,
            provider_name,
        )
    }

    /// Create a provider with custom settings
    fn new(
        api_key: String,
        base_url: String,
        chat_path: &str,
        models_path: &str,
        default_model: String,
        provider_name: String,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .context("Failed to create HTTP client")?;

        let endpoints = ProviderEndpoints::new(&base_url, chat_path, models_path);
        let canonical_openai_endpoint = provider_name == "openai"
            && endpoints.chat_url == "https://api.openai.com/v1/chat/completions"
            && endpoints.models_url == "https://api.openai.com/v1/models";

        Ok(Self {
            client,
            api_key,
            endpoints,
            default_model,
            provider_name,
            reasoning_effort: None,
            canonical_openai_endpoint,
        })
    }

    fn transport_rule(&self, model: &str) -> TransportRule {
        if self.canonical_openai_endpoint && matches!(model, "gpt-5.6-sol" | "gpt-5.6") {
            return TransportRule::CanonicalGpt56ChatCompletions;
        }
        TransportRule::CompatibleChatCompletions
    }

    /// Convert a Finch request according to the explicitly selected wire rule.
    fn to_openai_request(&self, request: &ProviderRequest) -> Result<OpenAIRequest> {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };
        let rule = self.transport_rule(&model);

        let mut messages: Vec<OpenAIMessage> = Vec::new();

        if let Some(system) = &request.system {
            messages.push(OpenAIMessage::Regular {
                role: match rule {
                    TransportRule::CanonicalGpt56ChatCompletions => "developer",
                    TransportRule::CompatibleChatCompletions => "system",
                }
                .to_string(),
                content: OpenAIMessageContent::Text(system.clone()),
            });
        }

        let mut outstanding_tool_ids = std::collections::HashSet::new();

        for msg in &request.messages {
            match msg.role.as_str() {
                "assistant" => {
                    // Collect text and tool_calls into a single assistant message.
                    // The OpenAI API requires tool_calls to be in the assistant message
                    // (not silently dropped), otherwise subsequent tool results are orphaned.
                    let text: String = msg
                        .content
                        .iter()
                        .filter_map(|b| b.as_text())
                        .collect::<Vec<_>>()
                        .join("");

                    let tool_calls: Vec<OpenAIRequestToolCall> = msg
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolUse { id, name, input } => {
                                let arguments = serde_json::to_string(input)
                                    .unwrap_or_else(|_| "{}".to_string());
                                Some(OpenAIRequestToolCall {
                                    id: id.clone(),
                                    tool_type: "function".to_string(),
                                    function: OpenAIRequestFunction {
                                        name: name.clone(),
                                        arguments,
                                    },
                                })
                            }
                            _ => None,
                        })
                        .collect();
                    if rule == TransportRule::CanonicalGpt56ChatCompletions {
                        for call in &tool_calls {
                            if call.id.is_empty() || call.function.name.is_empty() {
                                anyhow::bail!(
                                    "OpenAI function calls require non-empty IDs and names"
                                );
                            }
                            if !outstanding_tool_ids.insert(call.id.clone()) {
                                anyhow::bail!("Duplicate OpenAI function call ID '{}'", call.id);
                            }
                        }
                    }

                    // Grok (and strict OpenAI) require at least one of content or tool_calls.
                    // If both are absent, use a single space so the message is not empty.
                    let content = match (text.is_empty(), tool_calls.is_empty()) {
                        (false, _) => Some(text),
                        (true, false) => None, // tool_calls present — content optional
                        (true, true) => Some(" ".to_string()), // guard: never emit bare {"role":"assistant"}
                    };
                    messages.push(OpenAIMessage::Assistant {
                        role: "assistant".to_string(),
                        content,
                        tool_calls: if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls)
                        },
                    });
                }
                _ => {
                    // user/developer messages: keep ordered multimodal content for canonical
                    // OpenAI. Compatible providers retain Finch's historical text shape.
                    let mut content_parts: Vec<OpenAIContentPart> = Vec::new();
                    let mut compatible_text_parts: Vec<&str> = Vec::new();
                    let mut tool_results: Vec<(String, String)> = Vec::new();

                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                compatible_text_parts.push(text.as_str());
                                content_parts.push(OpenAIContentPart::Text { text: text.clone() });
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                ..
                            } => {
                                tool_results.push((tool_use_id.clone(), content.clone()));
                            }
                            ContentBlock::Image { source } => match rule {
                                TransportRule::CanonicalGpt56ChatCompletions => {
                                    content_parts.push(OpenAIContentPart::ImageUrl {
                                        image_url: validate_image_source(source)?,
                                    });
                                }
                                TransportRule::CompatibleChatCompletions => {
                                    compatible_text_parts.push("[image]");
                                }
                            },
                            ContentBlock::ToolUse { .. } => {}
                        }
                    }

                    let content = match rule {
                        TransportRule::CanonicalGpt56ChatCompletions => {
                            if content_parts.is_empty() {
                                None
                            } else {
                                Some(OpenAIMessageContent::Parts(content_parts))
                            }
                        }
                        TransportRule::CompatibleChatCompletions => {
                            let text = compatible_text_parts.join("\n");
                            (!text.trim().is_empty()).then_some(OpenAIMessageContent::Text(text))
                        }
                    };
                    if let Some(content) = content {
                        messages.push(OpenAIMessage::Regular {
                            role: msg.role.clone(),
                            content,
                        });
                    }

                    // One tool message per result (OpenAI requires separate messages)
                    for (tool_call_id, content) in tool_results {
                        if rule == TransportRule::CanonicalGpt56ChatCompletions
                            && !outstanding_tool_ids.remove(&tool_call_id)
                        {
                            anyhow::bail!(
                                "OpenAI tool result references unknown function call ID '{}'",
                                tool_call_id
                            );
                        }
                        messages.push(OpenAIMessage::Tool {
                            role: "tool".to_string(),
                            content: if content.trim().is_empty() {
                                "(no output)".to_string()
                            } else {
                                content
                            },
                            tool_call_id,
                        });
                    }
                }
            }
        }
        if rule == TransportRule::CanonicalGpt56ChatCompletions && !outstanding_tool_ids.is_empty()
        {
            anyhow::bail!("OpenAI request contained function calls without matching results");
        }

        // Convert tools to OpenAI format if present
        let tools = request.tools.as_ref().map(|tool_defs| {
            tool_defs
                .iter()
                .map(|tool| {
                    // Convert ToolInputSchema to Value
                    let parameters = match serde_json::to_value(&tool.input_schema) {
                        Ok(value) => value,
                        Err(e) => {
                            tracing::warn!(
                                "Failed to convert tool schema for '{}': {}",
                                tool.name,
                                e
                            );
                            serde_json::json!({})
                        }
                    };

                    OpenAITool {
                        tool_type: "function".to_string(),
                        function: OpenAIFunction {
                            name: tool.name.clone(),
                            description: tool.description.clone(),
                            parameters,
                        },
                    }
                })
                .collect()
        });

        let openai_request = OpenAIRequest {
            model,
            messages,
            max_tokens: (rule == TransportRule::CompatibleChatCompletions)
                .then_some(request.max_tokens),
            max_completion_tokens: (rule == TransportRule::CanonicalGpt56ChatCompletions)
                .then_some(request.max_tokens),
            temperature: request.temperature,
            reasoning_effort: self.reasoning_effort.map(ReasoningEffort::as_str),
            tools,
            stream: request.stream,
            stream_options: (request.stream
                && rule == TransportRule::CanonicalGpt56ChatCompletions)
                .then_some(OpenAIStreamOptions {
                    include_usage: true,
                }),
        };
        let encoded =
            serde_json::to_vec(&openai_request).context("Failed to serialize OpenAI request")?;
        if encoded.len() > MAX_REQUEST_BYTES {
            anyhow::bail!("OpenAI request exceeded the 32 MiB payload limit");
        }
        Ok(openai_request)
    }

    /// Convert OpenAI response to ProviderResponse
    fn parse_response(
        &self,
        response: OpenAIResponse,
        rule: TransportRule,
    ) -> Result<ProviderResponse> {
        if rule == TransportRule::CanonicalGpt56ChatCompletions {
            if response.object.as_deref() != Some("chat.completion") {
                anyhow::bail!("OpenAI returned an unknown response object");
            }
            if response.model.trim().is_empty() {
                anyhow::bail!("OpenAI response omitted the actual model");
            }
            if response.choices.len() != 1 || response.choices[0].index != 0 {
                anyhow::bail!("OpenAI returned an unexpected choice set");
            }
        }
        let choice = response
            .choices
            .into_iter()
            .next()
            .context("OpenAI returned no choices in response")?;

        // Convert message content to ContentBlock
        let mut content = Vec::new();

        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                content.push(ContentBlock::Text { text });
            }
        }

        // Convert tool calls to ContentBlock::ToolUse
        if let Some(tool_calls) = choice.message.tool_calls {
            let mut call_ids = std::collections::HashSet::new();
            for tool_call in tool_calls {
                if tool_call.tool_type == "function" {
                    let input = match rule {
                        TransportRule::CanonicalGpt56ChatCompletions => {
                            if tool_call.id.is_empty()
                                || tool_call.function.name.is_empty()
                                || !call_ids.insert(tool_call.id.clone())
                            {
                                anyhow::bail!(
                                    "OpenAI returned an invalid or duplicate function call ID/name"
                                );
                            }
                            let input: serde_json::Value =
                                serde_json::from_str(&tool_call.function.arguments)
                                    .context("OpenAI returned malformed JSON function arguments")?;
                            if !input.is_object() {
                                anyhow::bail!("OpenAI function arguments were not a JSON object");
                            }
                            input
                        }
                        TransportRule::CompatibleChatCompletions => {
                            serde_json::from_str(&tool_call.function.arguments)
                                .unwrap_or_else(|_| serde_json::json!({}))
                        }
                    };
                    content.push(ContentBlock::ToolUse {
                        id: tool_call.id,
                        name: tool_call.function.name,
                        input,
                    });
                } else if rule == TransportRule::CanonicalGpt56ChatCompletions {
                    anyhow::bail!("OpenAI returned an unknown tool-call type");
                }
            }
        }

        if rule == TransportRule::CanonicalGpt56ChatCompletions {
            let reason = choice
                .finish_reason
                .as_deref()
                .context("OpenAI response omitted terminal status")?;
            if !matches!(reason, "stop" | "length" | "tool_calls" | "content_filter") {
                anyhow::bail!("OpenAI returned unknown terminal status '{}'", reason);
            }
        }

        Ok(ProviderResponse {
            id: response.id,
            model: response.model,
            content,
            stop_reason: choice.finish_reason,
            role: choice.message.role,
            provider: self.provider_name.clone(),
        })
    }

    /// Send a single message request (no retry)
    async fn send_message_once(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let openai_request = self.to_openai_request(request)?;
        let rule = self.transport_rule(&openai_request.model);
        let url = &self.endpoints.chat_url;

        tracing::debug!(
            provider = %self.provider_name,
            model = %openai_request.model,
            messages = openai_request.messages.len(),
            tools = openai_request.tools.as_ref().map_or(0, Vec::len),
            stream = openai_request.stream,
            "sending OpenAI-compatible request"
        );

        let response = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&openai_request)
            .send()
            .await
            .context("Failed to send request to OpenAI API")?;

        let status = response.status();

        if !status.is_success() {
            let msg = read_api_error(response, status, rule).await;
            if status.is_client_error() {
                return Err(anyhow::Error::new(NonRetriableError(msg)));
            }
            anyhow::bail!("{}", msg);
        }

        let openai_response: OpenAIResponse = match rule {
            TransportRule::CanonicalGpt56ChatCompletions => {
                let body = read_bounded_response_body(response).await?;
                let value: serde_json::Value =
                    serde_json::from_slice(&body).context("Failed to parse OpenAI API response")?;
                validate_canonical_response_shape(&value)?;
                serde_json::from_value(value)
                    .context("OpenAI response did not match the documented schema")?
            }
            TransportRule::CompatibleChatCompletions => response
                .json()
                .await
                .context("Failed to parse OpenAI API response")?,
        };

        tracing::debug!(
            provider = %self.provider_name,
            response_id = %openai_response.id,
            model = %openai_response.model,
            choices = openai_response.choices.len(),
            "received OpenAI-compatible response"
        );

        self.parse_response(openai_response, rule)
    }

    /// Send a message with streaming response (no retry)
    async fn send_message_stream_once(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let (tx, rx) = mpsc::channel(100);

        let mut openai_request = self.to_openai_request(request)?;
        openai_request.stream = true;
        let rule = self.transport_rule(&openai_request.model);
        if rule == TransportRule::CanonicalGpt56ChatCompletions {
            openai_request.stream_options = Some(OpenAIStreamOptions {
                include_usage: true,
            });
        }

        let url = &self.endpoints.chat_url;

        tracing::debug!("Sending streaming request to OpenAI API");

        let response = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&openai_request)
            .send()
            .await
            .context("Failed to send streaming request to OpenAI API")?;

        let status = response.status();
        if !status.is_success() {
            let msg = read_api_error(response, status, rule).await;
            if status.is_client_error() {
                return Err(anyhow::Error::new(NonRetriableError(msg)));
            }
            anyhow::bail!("{}", msg);
        }

        if rule == TransportRule::CanonicalGpt56ChatCompletions {
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            if !content_type.starts_with("text/event-stream") {
                anyhow::bail!("OpenAI streaming response was not text/event-stream");
            }
            return Ok(spawn_canonical_stream_parser(response));
        }

        // Compatible providers retain the permissive historical parser.
        tokio::spawn(async move {
            tracing::debug!("[STREAM] OpenAI streaming task started");
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut accumulated_text = String::new();
            // Tool call accumulator: indexed by tool_call.index.
            // Each entry: (call_id, function_name, arguments_so_far).
            // Converted to ContentBlock::ToolUse when [DONE] arrives.
            let mut tool_call_acc: Vec<(String, String, String)> = Vec::new();
            #[allow(unused_assignments)]
            let mut done = false;

            while let Some(chunk) = stream.next().await {
                if done {
                    break;
                }

                match chunk {
                    Ok(bytes) => {
                        buffer.extend_from_slice(&bytes);

                        // Parse line by line
                        while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
                            let line_bytes: Vec<u8> = buffer.drain(..=newline_pos).collect();
                            let line = String::from_utf8_lossy(&line_bytes);

                            // SSE format: "data: {...}\n"
                            if let Some(json_str) = line.strip_prefix("data: ") {
                                let json_str = json_str.trim();

                                // Check for end marker
                                if json_str == "[DONE]" {
                                    tracing::debug!("[STREAM] Received [DONE]");

                                    // Send accumulated text as final block
                                    if !accumulated_text.is_empty() {
                                        let block = ContentBlock::Text {
                                            text: accumulated_text.clone(),
                                        };
                                        if tx
                                            .send(Ok(StreamChunk::ContentBlockComplete(block)))
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }

                                    // Convert accumulated tool call deltas to ToolUse blocks
                                    let blocks = match finalize_tool_calls(&tool_call_acc, false) {
                                        Ok(blocks) => blocks,
                                        Err(error) => {
                                            let _ = tx.send(Err(error)).await;
                                            done = true;
                                            break;
                                        }
                                    };
                                    for block in blocks {
                                        if let ContentBlock::ToolUse {
                                            ref name, ref id, ..
                                        } = block
                                        {
                                            tracing::debug!(
                                                "[STREAM] Sending tool call: {} ({})",
                                                name,
                                                id
                                            );
                                        }
                                        if tx
                                            .send(Ok(StreamChunk::ContentBlockComplete(block)))
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }

                                    done = true;
                                    break;
                                }

                                // Parse streaming chunk
                                if let Ok(stream_chunk) =
                                    serde_json::from_str::<OpenAIStreamChunk>(json_str)
                                {
                                    if let Some(choice) = stream_chunk.choices.into_iter().next() {
                                        if let Some(content) = choice.delta.content {
                                            accumulated_text.push_str(&content);
                                            // Send delta immediately
                                            if tx
                                                .send(Ok(StreamChunk::TextDelta(content)))
                                                .await
                                                .is_err()
                                            {
                                                done = true;
                                                break;
                                            }
                                        }

                                        // Accumulate tool call deltas — OpenAI sends them piecemeal.
                                        // Each delta may contain partial id/name/arguments fragments
                                        // for each tool call (identified by index).
                                        if let Some(tc_deltas) = choice.delta.tool_calls {
                                            for tc in tc_deltas {
                                                accumulate_tool_call_delta(&mut tool_call_acc, &tc);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Stream error: {}", e);
                        let _ = tx.send(Err(e.into())).await;
                        break;
                    }
                }
            }

            tracing::debug!("[STREAM] OpenAI streaming task finished");
        });

        Ok(rx)
    }
}

#[async_trait]
impl ProviderBackend for OpenAIProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let request = request.into_request_for(self)?;
        with_retry(|| self.send_message_once(&request)).await
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let request = request.into_request_for(self)?;
        with_retry(|| self.send_message_stream_once(&request)).await
    }

    fn name(&self) -> &str {
        &self.provider_name
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        let canonical_endpoints = match self.provider_name.as_str() {
            "openai" => {
                self.endpoints.chat_url == "https://api.openai.com/v1/chat/completions"
                    && self.endpoints.models_url == "https://api.openai.com/v1/models"
            }
            "grok" => {
                self.endpoints.chat_url == "https://api.x.ai/v1/chat/completions"
                    && self.endpoints.models_url == "https://api.x.ai/v1/models"
            }
            "mistral" => {
                self.endpoints.chat_url == "https://api.mistral.ai/v1/chat/completions"
                    && self.endpoints.models_url == "https://api.mistral.ai/v1/models"
            }
            "groq" => {
                self.endpoints.chat_url == "https://api.groq.com/openai/v1/chat/completions"
                    && self.endpoints.models_url == "https://api.groq.com/openai/v1/models"
            }
            _ => false,
        };
        if !canonical_endpoints {
            return ModelCapabilities::unknown(self.name(), model);
        }

        let (source, streaming, tools, reasoning, max_tokens, max_output_tokens) =
            match (self.provider_name.as_str(), model) {
                ("openai", "gpt-5.6-sol" | "gpt-5.6") => (
                    "https://developers.openai.com/api/docs/models/gpt-5.6-sol",
                    CapabilitySupport::Supported,
                    CapabilitySupport::Supported,
                    ReasoningCapability::allowed(
                        [
                            ReasoningEffort::None,
                            ReasoningEffort::Low,
                            ReasoningEffort::Medium,
                            ReasoningEffort::High,
                            ReasoningEffort::Xhigh,
                            ReasoningEffort::Max,
                        ],
                        "2026-08-26",
                        "https://developers.openai.com/api/docs/models/gpt-5.6-sol",
                    ),
                    1_050_000,
                    Some(128_000),
                ),
                ("openai", "gpt-4o") => (
                    "https://developers.openai.com/api/docs/models/gpt-4o",
                    CapabilitySupport::Supported,
                    CapabilitySupport::Supported,
                    ReasoningCapability::unsupported(
                        "2026-08-26",
                        "https://developers.openai.com/api/docs/models/gpt-4o",
                    ),
                    128_000,
                    Some(16_384),
                ),
                ("grok", "grok-4.6") => (
                    "https://docs.x.ai/developers/grok-4-6; https://docs.x.ai/developers/model-capabilities/text/streaming",
                    CapabilitySupport::Supported,
                    CapabilitySupport::Supported,
                    ReasoningCapability::allowed(
                        [
                            ReasoningEffort::Low,
                            ReasoningEffort::Medium,
                            ReasoningEffort::High,
                            ReasoningEffort::Xhigh,
                        ],
                        "2026-08-26",
                        "https://docs.x.ai/developers/grok-4-6",
                    ),
                    500_000,
                    None,
                ),
                ("mistral", "mistral-large-2512") => (
                    "https://docs.mistral.ai/models/mistral-large-3-25-12",
                    CapabilitySupport::Unknown,
                    CapabilitySupport::Supported,
                    ReasoningCapability::unknown(),
                    256_000,
                    None,
                ),
                ("groq", "openai/gpt-oss-120b") => (
                    "https://console.groq.com/docs/model/openai/gpt-oss-120b; https://console.groq.com/docs/production-readiness/optimizing-latency",
                    CapabilitySupport::Supported,
                    CapabilitySupport::Supported,
                    ReasoningCapability::allowed(
                        [
                            ReasoningEffort::Low,
                            ReasoningEffort::Medium,
                            ReasoningEffort::High,
                        ],
                        "2026-08-26",
                        "https://console.groq.com/docs/model/openai/gpt-oss-120b",
                    ),
                    131_072,
                    Some(65_536),
                ),
                // Ollama and remote-daemon catalogs are deployment-specific;
                // all optional capabilities remain unknown until attested.
                _ => return ModelCapabilities::unknown(self.name(), model),
            };
        let mut capabilities = ModelCapabilities::static_metadata(
            self.name(),
            model,
            "2026-08-26",
            source,
            streaming,
            tools,
            CapabilitySupport::Unsupported,
            reasoning,
            Some(max_tokens),
            max_output_tokens,
            None,
        )
        .with_wire_protocol(
            WireProtocol::OpenAiChatCompletions,
            "2026-08-26",
            "Finch OpenAI-compatible chat-completions adapter",
        );
        if self.provider_name == "openai" && matches!(model, "gpt-5.6-sol" | "gpt-5.6") {
            capabilities.image_input = ModelFeature::static_metadata(
                CapabilitySupport::Supported,
                "2026-08-27",
                "https://developers.openai.com/api/docs/models/gpt-5.6-sol",
            );
        }
        capabilities
    }

    fn requested_reasoning_effort(&self, _request: &ProviderRequest) -> Option<ReasoningEffort> {
        self.reasoning_effort
    }
}

// OpenAI API types

#[derive(Debug, Clone, Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAITool>>,
    #[serde(skip_serializing_if = "is_false")]
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<OpenAIStreamOptions>,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIStreamOptions {
    include_usage: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// OpenAI message format — request side only (we never deserialize this)
///
/// The untagged variants are ordered so serde tries the most-specific first:
/// Tool (has tool_call_id), Assistant (has optional tool_calls), then Regular.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum OpenAIMessage {
    /// Tool result message (one per tool invocation)
    Tool {
        role: String, // "tool"
        content: String,
        tool_call_id: String,
    },
    /// Assistant message — may contain text, tool_calls, or both
    Assistant {
        role: String, // "assistant"
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<OpenAIRequestToolCall>>,
    },
    /// Plain user / system/developer message
    Regular {
        role: String,
        content: OpenAIMessageContent,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum OpenAIMessageContent {
    Text(String),
    Parts(Vec<OpenAIContentPart>),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAIContentPart {
    Text { text: String },
    ImageUrl { image_url: OpenAIImageUrl },
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIImageUrl {
    url: String,
}

/// Tool call entry inside an assistant message (request format)
#[derive(Debug, Clone, Serialize)]
struct OpenAIRequestToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIRequestFunction,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIRequestFunction {
    name: String,
    arguments: String, // JSON-encoded string
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAITool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAIFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIResponse {
    id: String,
    #[serde(default)]
    object: Option<String>,
    model: String,
    choices: Vec<OpenAIChoice>,
    #[serde(default)]
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenAIChoice {
    index: usize,
    message: OpenAIResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponseMessage {
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIToolFunction,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIToolFunction {
    name: String,
    arguments: String, // JSON string
}

// Streaming types

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenAIStreamChunk {
    id: String,
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    model: String,
    choices: Vec<OpenAIStreamChoice>,
    #[serde(default)]
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    #[allow(dead_code)]
    completion_tokens: u32,
    #[allow(dead_code)]
    total_tokens: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenAIStreamChoice {
    index: usize,
    delta: OpenAIDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenAIDelta {
    role: Option<String>,
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCallDelta>>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenAIToolCallDelta {
    index: Option<usize>,
    id: Option<String>,
    #[serde(rename = "type")]
    tool_type: Option<String>,
    function: Option<OpenAIFunctionDelta>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedLogs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn canonical_test_provider(base_url: String) -> OpenAIProvider {
        let mut provider = OpenAIProvider::new_compatible(
            "test-secret".to_string(),
            base_url,
            "/v1/chat/completions",
            "/v1/models",
            "gpt-5.6-sol".to_string(),
            "openai".to_string(),
        )
        .unwrap()
        .with_reasoning_effort(ReasoningEffort::High);
        provider.canonical_openai_endpoint = true;
        provider
    }

    async fn stalling_http_server(
        send_sse_headers: bool,
    ) -> (String, tokio::sync::oneshot::Receiver<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 16 * 1024];
            let _ = socket.read(&mut request).await;
            if send_sse_headers {
                socket.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                ).await.unwrap();
                socket.flush().await.unwrap();
            }
            let mut byte = [0u8; 1];
            loop {
                match socket.read(&mut byte).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = closed_tx.send(());
        });
        (format!("http://{}", address), closed_rx)
    }

    async fn canonical_stream_outcome(body: String) -> (bool, Vec<String>) {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;
        let provider = canonical_test_provider(server.url());
        let mut rx = provider
            .send_message_stream_once(
                &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                    .with_model("gpt-5.6-sol"),
            )
            .await
            .unwrap();
        let mut complete = false;
        let mut errors = Vec::new();
        while let Some(item) = rx.recv().await {
            match item {
                Ok(StreamChunk::ContentBlockComplete(_)) => complete = true,
                Err(error) => errors.push(error.to_string()),
                _ => {}
            }
        }
        (complete, errors)
    }

    #[tokio::test]
    async fn canonical_gpt_5_6_posts_exact_current_chat_completions_json() {
        use crate::claude::types::{ContentBlock, Message};
        use crate::tools::types::{ToolDefinition, ToolInputSchema};

        let mut server = mockito::Server::new_async().await;
        let expected = serde_json::json!({
            "model": "gpt-5.6-sol",
            "messages": [
                {"role":"developer","content":"guard"},
                {"role":"user","content":[
                    {"type":"text","text":"inspect"},
                    {"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgo="}},
                    {"type":"image_url","image_url":{"url":"data:image/jpeg;base64,/9j/"}}
                ]},
                {"role":"assistant","tool_calls":[
                    {"id":"call_a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}},
                    {"id":"call_b","type":"function","function":{"name":"read","arguments":"{\"path\":\"b\"}"}}
                ]},
                {"role":"tool","content":"A","tool_call_id":"call_a"},
                {"role":"tool","content":"B","tool_call_id":"call_b"}
            ],
            "max_completion_tokens": 321,
            "reasoning_effort": "high",
            "tools": [{"type":"function","function":{
                "name":"read","description":"read file","parameters":{
                    "type":"object","properties":{"path":{"type":"string","description":"path"}},"required":["path"]
                }
            }}]
        });
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer test-secret")
            .match_body(mockito::Matcher::Json(expected))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"chat-1","object":"chat.completion","model":"gpt-5.6-sol-2026-08-01","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":1,"total_tokens":10}}"#)
            .create_async()
            .await;
        let provider = canonical_test_provider(server.url());
        let request = ProviderRequest::new(vec![
            Message::with_content(
                "user",
                vec![
                    ContentBlock::text("inspect"),
                    ContentBlock::image("image/png", "iVBORw0KGgo="),
                    ContentBlock::image("image/jpeg", "/9j/"),
                ],
            ),
            Message::with_content(
                "assistant",
                vec![
                    ContentBlock::ToolUse {
                        id: "call_a".into(),
                        name: "read".into(),
                        input: serde_json::json!({"path":"a"}),
                    },
                    ContentBlock::ToolUse {
                        id: "call_b".into(),
                        name: "read".into(),
                        input: serde_json::json!({"path":"b"}),
                    },
                ],
            ),
            Message::with_content(
                "user",
                vec![
                    ContentBlock::tool_result("call_a".into(), "A".into(), None),
                    ContentBlock::tool_result("call_b".into(), "B".into(), None),
                ],
            ),
        ])
        .with_model("gpt-5.6-sol")
        .with_system("guard")
        .with_max_tokens(321)
        .with_tools(vec![ToolDefinition {
            name: "read".into(),
            description: "read file".into(),
            input_schema: ToolInputSchema::simple(vec![("path", "path")]),
        }]);
        let response = provider.send_message_once(&request).await.unwrap();
        assert_eq!(response.model, "gpt-5.6-sol-2026-08-01");
        mock.assert_async().await;
    }

    #[test]
    fn canonical_and_compatible_rules_are_explicit_and_separate() {
        let canonical = OpenAIProvider::new_openai("key".into()).unwrap();
        assert_eq!(
            canonical.transport_rule("gpt-5.6-sol"),
            TransportRule::CanonicalGpt56ChatCompletions
        );
        assert_eq!(
            canonical.transport_rule("gpt-5.6"),
            TransportRule::CanonicalGpt56ChatCompletions
        );
        assert_eq!(
            canonical.transport_rule("gpt-4o"),
            TransportRule::CompatibleChatCompletions
        );
        let custom = OpenAIProvider::new_compatible(
            "key".into(),
            "https://gateway.example".into(),
            "/v1/chat/completions",
            "/v1/models",
            "gpt-5.6-sol".into(),
            "openai".into(),
        )
        .unwrap();
        assert_eq!(
            custom.transport_rule("gpt-5.6-sol"),
            TransportRule::CompatibleChatCompletions
        );
        assert_eq!(
            canonical.capabilities("gpt-5.6-sol").wire_protocol.protocol,
            Some(WireProtocol::OpenAiChatCompletions)
        );
        assert!(canonical
            .capabilities("gpt-5.6-sol")
            .image_input
            .is_supported());
        assert!(canonical.capabilities("gpt-5.6").image_input.is_supported());
        let alias = canonical
            .clone()
            .with_model("gpt-5.6")
            .with_reasoning_effort(ReasoningEffort::High);
        let validated = crate::providers::validate_provider_request(
            &alias,
            &ProviderRequest::new(vec![crate::claude::Message::with_content(
                "user",
                vec![ContentBlock::image("image/png", "iVBORw0KGgo=")],
            )]),
            true,
        )
        .unwrap();
        assert_eq!(validated.capabilities().model, "gpt-5.6");
    }

    #[tokio::test]
    async fn canonical_stream_preserves_fragmented_parallel_calls_usage_and_terminal() {
        let mut server = mockito::Server::new_async().await;
        let body = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol-actual\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"pa\"}},{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol-actual\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a\\\"}\"}},{\"index\":1,\"function\":{\"arguments\":\"th\\\":\\\"b\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol-actual\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol-actual\",\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}\n\n",
            "data: [DONE]\n\n"
        );
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model":"gpt-5.6-sol", "stream":true,
                "stream_options":{"include_usage":true}, "max_completion_tokens":4096
            })))
            .with_status(200)
            .with_header("content-type", "text/event-stream; charset=utf-8")
            .with_body(body)
            .create_async()
            .await;
        let provider = canonical_test_provider(server.url());
        let mut rx = provider
            .send_message_stream_once(
                &ProviderRequest::new(vec![crate::claude::Message::user("use tools")])
                    .with_model("gpt-5.6-sol"),
            )
            .await
            .unwrap();
        let mut calls = Vec::new();
        let mut usage = None;
        while let Some(item) = rx.recv().await {
            match item.unwrap() {
                StreamChunk::ContentBlockComplete(ContentBlock::ToolUse { id, name, input }) => {
                    calls.push((id, name, input))
                }
                StreamChunk::Usage { input_tokens } => usage = Some(input_tokens),
                _ => {}
            }
        }
        assert_eq!(usage, Some(12));
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "call_a");
        assert_eq!(calls[0].2, serde_json::json!({"path":"a"}));
        assert_eq!(calls[1].0, "call_b");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn canonical_stream_rejects_premature_eof_at_http_boundary() {
        let mut server = mockito::Server::new_async().await;
        server.mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n")
            .create_async().await;
        let provider = canonical_test_provider(server.url());
        let mut rx = provider
            .send_message_stream_once(
                &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                    .with_model("gpt-5.6-sol"),
            )
            .await
            .unwrap();
        let mut error = None;
        while let Some(item) = rx.recv().await {
            if let Err(err) = item {
                error = Some(err.to_string());
            }
        }
        assert_eq!(
            error.as_deref(),
            Some("OpenAI stream reached EOF before [DONE]")
        );
    }

    #[tokio::test]
    async fn canonical_stream_releases_transport_when_receiver_is_dropped() {
        let (url, closed) = stalling_http_server(true).await;
        let provider = canonical_test_provider(url);
        let rx = provider
            .send_message_stream_once(
                &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                    .with_model("gpt-5.6-sol"),
            )
            .await
            .unwrap();
        drop(rx);
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("transport was not released after receiver drop")
            .unwrap();
    }

    #[tokio::test]
    async fn canonical_request_timeout_is_bounded_and_releases_transport() {
        let (url, closed) = stalling_http_server(false).await;
        let mut provider = canonical_test_provider(url);
        provider.client = Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let error = provider
            .send_message_once(
                &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                    .with_model("gpt-5.6-sol"),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Failed to send request"));
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("timed-out transport was not released")
            .unwrap();
    }

    #[tokio::test]
    async fn canonical_stream_rejects_duplicate_or_late_done_without_completion_block() {
        for trailing in [
            "data: [DONE]\n\n",
            "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        ] {
            let mut server = mockito::Server::new_async().await;
            let body = format!(
                "data: {{\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"provisional\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n{}",
                trailing
            );
            server
                .mock("POST", "/v1/chat/completions")
                .with_status(200)
                .with_header("content-type", "text/event-stream")
                .with_body(body)
                .create_async()
                .await;
            let provider = canonical_test_provider(server.url());
            let mut rx = provider
                .send_message_stream_once(
                    &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                        .with_model("gpt-5.6-sol"),
                )
                .await
                .unwrap();
            let mut complete = false;
            let mut terminal_error = false;
            while let Some(item) = rx.recv().await {
                match item {
                    Ok(StreamChunk::ContentBlockComplete(_)) => complete = true,
                    Err(error) => {
                        terminal_error = error.to_string().contains("after its terminal marker")
                    }
                    _ => {}
                }
            }
            assert!(terminal_error);
            assert!(!complete, "completion published before terminal uniqueness was proven");
        }
    }

    #[tokio::test]
    async fn canonical_stream_enforces_sse_field_line_and_total_bounds_at_http_boundary() {
        let (complete, errors) = canonical_stream_outcome("event: mystery\n\n".to_string()).await;
        assert!(!complete);
        assert!(errors
            .iter()
            .any(|error| error.contains("unknown SSE field")));

        let unknown_delta = "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{\"unknown_item\":{}},\"finish_reason\":null}]}\n\n";
        let (complete, errors) = canonical_stream_outcome(unknown_delta.to_string()).await;
        assert!(!complete);
        assert!(errors
            .iter()
            .any(|error| error.contains("unknown delta field")));

        let mut oversized_line = concat!(
            "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
            ": short\n"
        )
        .as_bytes()
        .to_vec();
        oversized_line.extend(std::iter::repeat_n(b'a', MAX_SSE_LINE_BYTES));
        oversized_line.push(b'\n');
        let (complete, errors) =
            canonical_stream_outcome(String::from_utf8(oversized_line).unwrap()).await;
        assert!(!complete);
        assert!(errors.iter().any(|error| error.contains("line exceeded")));

        let comment = format!(":{}\n", "a".repeat(900_000));
        let oversized_total = comment.repeat(5);
        let (complete, errors) = canonical_stream_outcome(oversized_total).await;
        assert!(!complete);
        assert!(errors.iter().any(|error| error.contains("total limit")));

        assert!(!sse_line_prefix_exceeds_limit(&vec![
            b'a';
            MAX_SSE_LINE_BYTES
        ]));
        assert!(sse_line_prefix_exceeds_limit(&vec![
            b'a';
            MAX_SSE_LINE_BYTES + 1
        ]));
    }

    #[test]
    fn canonical_stream_rejects_unknown_malformed_duplicate_and_mismatched_events() {
        let mut state = CanonicalStreamState::default();
        assert!(canonical_stream_data(&mut state, "not-json")
            .unwrap_err()
            .to_string()
            .contains("malformed JSON"));
        assert!(canonical_stream_data(
            &mut state,
            r#"{"id":"x","object":"response.output_text.delta","model":"gpt-5.6-sol","choices":[]}"#
        )
        .unwrap_err()
        .to_string()
        .contains("unknown event"));
        let mut state = CanonicalStreamState::default();
        assert!(canonical_stream_data(
            &mut state,
            r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{"mystery":"payload"},"finish_reason":null}]}"#
        )
        .unwrap_err()
        .to_string()
        .contains("unknown delta field"));
        let mut state = CanonicalStreamState::default();
        assert!(canonical_stream_data(
            &mut state,
            r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{"role":"tool"},"finish_reason":null}]}"#
        )
        .unwrap_err()
        .to_string()
        .contains("unknown delta role"));
        let mut state = CanonicalStreamState::default();
        assert!(canonical_stream_data(
            &mut state,
            r#"{"id":"x","object":"chat.completion.chunk","model":"","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#
        )
        .unwrap_err()
        .to_string()
        .contains("omitted the actual model"));
        let mut state = CanonicalStreamState::default();
        canonical_stream_data(
            &mut state,
            r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol-a","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
        )
        .unwrap();
        assert!(canonical_stream_data(
            &mut state,
            r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol-b","choices":[{"index":0,"delta":{"content":"x"},"finish_reason":null}]}"#
        )
        .unwrap_err()
        .to_string()
        .contains("changed actual model"));
        let mut state = CanonicalStreamState::default();
        canonical_stream_data(&mut state, r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#).unwrap();
        assert!(canonical_stream_data(&mut state, r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#).unwrap_err().to_string().contains("duplicate terminal"));
        let mut state = CanonicalStreamState::default();
        canonical_stream_data(&mut state, r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read","arguments":"{}"}}]},"finish_reason":null}]}"#).unwrap();
        assert!(canonical_stream_data(&mut state, r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_b","function":{"arguments":""}}]},"finish_reason":null}]}"#).unwrap_err().to_string().contains("changed a function-call ID"));
    }

    #[test]
    fn canonical_images_and_tool_results_fail_closed_before_http() {
        let provider = canonical_test_provider("http://127.0.0.1:1".into());
        let bad_base64 = ProviderRequest::new(vec![crate::claude::Message::with_content(
            "user",
            vec![ContentBlock::image("image/png", "not base64")],
        )])
        .with_model("gpt-5.6-sol");
        assert!(provider
            .to_openai_request(&bad_base64)
            .unwrap_err()
            .to_string()
            .contains("invalid base64"));
        let bad_mime = ProviderRequest::new(vec![crate::claude::Message::with_content(
            "user",
            vec![ContentBlock::image("image/webp", "AAAA")],
        )])
        .with_model("gpt-5.6-sol");
        assert!(provider
            .to_openai_request(&bad_mime)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
        let mismatch = ProviderRequest::new(vec![crate::claude::Message::with_content(
            "user",
            vec![ContentBlock::tool_result(
                "missing".into(),
                "x".into(),
                None,
            )],
        )])
        .with_model("gpt-5.6-sol");
        assert!(provider
            .to_openai_request(&mismatch)
            .unwrap_err()
            .to_string()
            .contains("unknown function call ID"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn canonical_non_success_body_and_sensitive_request_fields_are_redacted_from_logs() {
        let mut server = mockito::Server::new_async().await;
        let upstream_secret = "UPSTREAM_REFLECTED_SECRET";
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(400)
            .with_body(format!(
                "{{\"error\":{{\"message\":\"{}{}\"}}}}",
                upstream_secret,
                "x".repeat(MAX_ERROR_BODY_BYTES + 1024)
            ))
            .create_async()
            .await;
        let captured = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&captured);
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || CapturedLogs(Arc::clone(&writer)))
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let provider = canonical_test_provider(server.url());
        let prompt_secret = "PROMPT_PRIVATE_VALUE";
        let request = ProviderRequest::new(vec![crate::claude::Message::with_content(
            "user",
            vec![
                ContentBlock::text(prompt_secret),
                ContentBlock::image("image/png", "iVBORw0KGgo="),
            ],
        )])
        .with_model("gpt-5.6-sol");
        let error = provider.send_message_once(&request).await.unwrap_err();
        let error = error.to_string();
        assert!(error.contains("response body redacted"));
        assert!(!error.contains(upstream_secret));
        let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        for secret in [
            "test-secret",
            upstream_secret,
            prompt_secret,
            "iVBORw0KGgo=",
        ] {
            assert!(
                !logs.contains(secret),
                "logs exposed sensitive request material"
            );
        }
    }

    #[tokio::test]
    async fn canonical_nonstream_rejects_malformed_status_model_and_unknown_items() {
        let cases = [
            ("not-json", "Failed to parse OpenAI API response"),
            (
                r#"{"id":"x","object":"chat.completion","model":"","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
                "omitted the actual model",
            ),
            (
                r#"{"id":"x","object":"chat.completion","model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"mystery"}]}"#,
                "unknown terminal status",
            ),
            (
                r#"{"id":"x","object":"chat.completion","model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"assistant","content":"ok","mystery_item":{}},"finish_reason":"stop"}]}"#,
                "unknown response message field",
            ),
            (
                r#"{"id":"x","object":"chat.completion","model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_x","type":"function","function":{"name":"read","arguments":"not-json"}}]},"finish_reason":"tool_calls"}]}"#,
                "malformed JSON function arguments",
            ),
        ];
        for (body, expected) in cases {
            let mut server = mockito::Server::new_async().await;
            server
                .mock("POST", "/v1/chat/completions")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(body)
                .create_async()
                .await;
            let provider = canonical_test_provider(server.url());
            let error = provider
                .send_message_once(
                    &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                        .with_model("gpt-5.6-sol"),
                )
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?}, got {error:#}"
            );
        }
    }

    #[tokio::test]
    async fn compatible_nonstream_keeps_historical_malformed_tool_argument_fallback() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(r#"{"id":"x","model":"compatible-model","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_x","type":"function","function":{"name":"read","arguments":"not-json"}}]},"finish_reason":"tool_calls"}]}"#)
            .create_async()
            .await;
        let provider = OpenAIProvider::new_compatible(
            "key".into(),
            server.url(),
            "/v1/chat/completions",
            "/v1/models",
            "compatible-model".into(),
            "compatible".into(),
        )
        .unwrap();
        let response = provider
            .send_message_once(&ProviderRequest::new(vec![crate::claude::Message::user(
                "hello",
            )]))
            .await
            .unwrap();
        assert_eq!(response.tool_uses()[0].input, serde_json::json!({}));
    }

    #[tokio::test]
    async fn compatible_provider_posts_to_exact_chat_path_with_bearer_auth() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/coding/paas/v4/chat/completions")
            .match_header("authorization", "Bearer endpoint-secret")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model": "gpt-4o"
            })))
            .with_status(200)
            .with_body(r#"{"id":"chat-1","object":"chat.completion","created":1,"model":"custom-model","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#)
            .create_async()
            .await;
        let provider = OpenAIProvider::new_compatible(
            "endpoint-secret".to_string(),
            server.url(),
            "/api/coding/paas/v4/chat/completions",
            "/api/coding/paas/v4/models",
            "gpt-4o".to_string(),
            "openai".to_string(),
        )
        .unwrap();

        provider
            .send_message(&ProviderRequest::new(vec![
                crate::claude::types::Message::user("hello"),
            ]))
            .await
            .unwrap();
        mock.assert_async().await;
    }

    #[test]
    fn test_openai_provider_creation() {
        let provider = OpenAIProvider::new_openai("test-key".to_string());
        assert!(provider.is_ok());
    }

    #[test]
    fn test_grok_provider_creation() {
        let provider = OpenAIProvider::new_grok("test-key".to_string());
        assert!(provider.is_ok());
    }

    #[test]
    fn test_provider_names() {
        let openai = OpenAIProvider::new_openai("test-key".to_string()).unwrap();
        assert_eq!(openai.name(), "openai");

        let grok = OpenAIProvider::new_grok("test-key".to_string()).unwrap();
        assert_eq!(grok.name(), "grok");
    }

    #[test]
    fn test_default_models() {
        let openai = OpenAIProvider::new_openai("key".to_string()).unwrap();
        assert!(!openai.default_model().is_empty());

        let grok = OpenAIProvider::new_grok("key".to_string()).unwrap();
        assert!(grok.default_model().contains("grok"));
    }

    #[test]
    fn test_to_openai_request_system_prompt() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::claude::types::Message;
        use crate::providers::types::ProviderRequest;
        let req =
            ProviderRequest::new(vec![Message::user("hello")]).with_system("You are helpful.");
        let openai_req = provider.to_openai_request(&req).unwrap();
        // System message should be first
        assert!(
            matches!(&openai_req.messages[0], OpenAIMessage::Regular { role, .. } if role == "system")
        );
        if let OpenAIMessage::Regular { content, .. } = &openai_req.messages[0] {
            assert!(
                matches!(content, OpenAIMessageContent::Text(text) if text == "You are helpful.")
            );
        }
    }

    #[test]
    fn test_to_openai_request_no_system_prompt() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::claude::types::Message;
        use crate::providers::types::ProviderRequest;
        let req = ProviderRequest::new(vec![Message::user("hello")]);
        let openai_req = provider.to_openai_request(&req).unwrap();
        // No system message — first message is user
        assert!(
            matches!(&openai_req.messages[0], OpenAIMessage::Regular { role, .. } if role == "user")
        );
    }

    #[test]
    fn test_to_openai_request_tool_calls_included() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::claude::types::{ContentBlock, Message};
        use crate::providers::types::ProviderRequest;
        let req = ProviderRequest::new(vec![
            Message::user("run ls"),
            Message::with_content(
                "assistant",
                vec![ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                }],
            ),
        ]);
        let openai_req = provider.to_openai_request(&req).unwrap();
        // Assistant message should have tool_calls
        let assistant_msg = openai_req
            .messages
            .iter()
            .find(|m| matches!(m, OpenAIMessage::Assistant { .. }));
        assert!(assistant_msg.is_some());
        if let Some(OpenAIMessage::Assistant { tool_calls, .. }) = assistant_msg {
            let calls = tool_calls.as_ref().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "call_1");
            assert_eq!(calls[0].function.name, "bash");
        }
    }

    #[test]
    fn test_to_openai_request_tool_result_becomes_tool_role() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::claude::types::{ContentBlock, Message};
        use crate::providers::types::ProviderRequest;
        let req = ProviderRequest::new(vec![
            Message::user("run ls"),
            Message::with_content(
                "assistant",
                vec![ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({}),
                }],
            ),
            Message::with_content(
                "user",
                vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "file.txt".to_string(),
                    is_error: None,
                }],
            ),
        ]);
        let openai_req = provider.to_openai_request(&req).unwrap();
        // There should be a "tool" role message
        let tool_msg = openai_req
            .messages
            .iter()
            .find(|m| matches!(m, OpenAIMessage::Tool { .. }));
        assert!(tool_msg.is_some());
        if let Some(OpenAIMessage::Tool {
            tool_call_id,
            content,
            ..
        }) = tool_msg
        {
            assert_eq!(tool_call_id, "call_1");
            assert_eq!(content, "file.txt");
        }
    }

    #[test]
    fn test_empty_tool_result_gets_placeholder() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::claude::types::{ContentBlock, Message};
        use crate::providers::types::ProviderRequest;
        let req = ProviderRequest::new(vec![Message::with_content(
            "user",
            vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "  ".to_string(), // whitespace-only
                is_error: None,
            }],
        )]);
        let openai_req = provider.to_openai_request(&req).unwrap();
        if let Some(OpenAIMessage::Tool { content, .. }) = openai_req
            .messages
            .iter()
            .find(|m| matches!(m, OpenAIMessage::Tool { .. }))
        {
            assert_eq!(content, "(no output)");
        } else {
            panic!("Expected a tool message");
        }
    }

    #[test]
    fn test_to_openai_request_empty_user_text_skipped() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::claude::types::{ContentBlock, Message};
        use crate::providers::types::ProviderRequest;
        // A user message with only whitespace text should not generate a "user" message
        let req = ProviderRequest::new(vec![Message::with_content(
            "user",
            vec![ContentBlock::Text {
                text: "   ".to_string(),
            }],
        )]);
        let openai_req = provider.to_openai_request(&req).unwrap();
        assert!(openai_req.messages.is_empty());
    }

    #[test]
    fn test_to_openai_request_uses_fallback_model() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::providers::types::ProviderRequest;
        // Request with empty model — should fall back to provider default
        let req = ProviderRequest::new(vec![]);
        let openai_req = provider.to_openai_request(&req).unwrap();
        assert!(!openai_req.model.is_empty());
    }

    #[test]
    fn test_to_openai_request_includes_reasoning_effort() {
        let provider = OpenAIProvider::new_openai("key".to_string())
            .unwrap()
            .with_model("gpt-5.6-sol")
            .with_reasoning_effort(ReasoningEffort::High);
        let request = ProviderRequest::new(vec![crate::claude::Message::user("reason carefully")]);
        let openai_request = provider.to_openai_request(&request).unwrap();

        assert_eq!(openai_request.model, "gpt-5.6-sol");
        assert_eq!(openai_request.reasoning_effort, Some("high"));
    }

    #[test]
    fn test_provider_supports_streaming() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        assert!(provider
            .capabilities(provider.default_model())
            .streaming
            .is_supported());
    }

    #[test]
    fn test_provider_supports_tools() {
        let provider = OpenAIProvider::new_grok("key".to_string()).unwrap();
        assert!(provider
            .capabilities(provider.default_model())
            .tools
            .is_supported());
    }

    #[test]
    fn same_provider_models_can_have_different_reasoning_capabilities() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        let sol = provider.capabilities("gpt-5.6-sol");
        let legacy = provider.capabilities("gpt-4o");
        assert_eq!(sol.provider, legacy.provider);
        assert_eq!(sol.reasoning.support(), CapabilitySupport::Supported);
        assert_eq!(legacy.reasoning.support(), CapabilitySupport::Unsupported);
        assert_eq!(
            provider
                .capabilities("vendor-private-model")
                .streaming
                .support,
            CapabilitySupport::Unknown
        );
    }

    #[test]
    fn deployment_specific_models_stay_unknown_without_runtime_attestation() {
        let ollama = OpenAIProvider::new_ollama(
            "http://localhost:11434".to_string(),
            "qwen2.5:7b".to_string(),
        )
        .unwrap();
        let remote =
            OpenAIProvider::new_remote_daemon("http://localhost:11435".to_string()).unwrap();
        assert_eq!(
            ollama
                .capabilities(ollama.default_model())
                .streaming
                .support,
            CapabilitySupport::Unknown
        );
        assert_eq!(
            remote
                .capabilities(remote.default_model())
                .streaming
                .support,
            CapabilitySupport::Unknown
        );
    }

    #[tokio::test]
    async fn configured_reasoning_rejects_ineligible_model_before_http() {
        let provider = OpenAIProvider::new_compatible(
            "key".to_string(),
            "http://127.0.0.1:1".to_string(),
            "/v1/chat/completions",
            "/v1/models",
            "gpt-4o".to_string(),
            "openai".to_string(),
        )
        .unwrap()
        .with_reasoning_effort(ReasoningEffort::High);
        let error = provider
            .send_message(&ProviderRequest::new(vec![]))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Provider 'openai' model 'gpt-4o' has unknown reasoning capability; refusing configured effort 'high'"
        );
    }

    #[tokio::test]
    async fn custom_endpoint_cannot_claim_canonical_openai_capabilities() {
        let provider = OpenAIProvider::new_compatible(
            "key".to_string(),
            "http://127.0.0.1:1".to_string(),
            "/v1/chat/completions",
            "/v1/models",
            "gpt-5.6-sol".to_string(),
            "openai".to_string(),
        )
        .unwrap()
        .with_reasoning_effort(ReasoningEffort::High);
        assert_eq!(
            provider.capabilities("gpt-5.6-sol").reasoning.support(),
            CapabilitySupport::Unknown
        );
        let error = provider
            .send_message(&ProviderRequest::new(vec![]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unknown reasoning capability"));
        assert!(!error.to_string().contains("Connection refused"));
    }

    #[tokio::test]
    async fn gpt_5_6_sol_rejects_minimal_reasoning_before_http() {
        let provider = OpenAIProvider::new_openai("key".to_string())
            .unwrap()
            .with_model("gpt-5.6-sol")
            .with_reasoning_effort(ReasoningEffort::Minimal);
        let error = provider
            .send_message(&ProviderRequest::new(vec![]))
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("does not support reasoning effort 'minimal'"));
    }

    #[test]
    fn gpt_5_6_sol_reasoning_efforts_are_exact() {
        let base = OpenAIProvider::new_openai("key".to_string()).unwrap();
        let capabilities = base.capabilities("gpt-5.6-sol");
        let allowed = vec![
            ReasoningEffort::None,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Xhigh,
            ReasoningEffort::Max,
        ];
        assert_eq!(
            capabilities.reasoning.allowed_efforts,
            Some(allowed.clone())
        );
        for effort in allowed {
            let provider = base
                .clone()
                .with_model("gpt-5.6-sol")
                .with_reasoning_effort(effort);
            crate::providers::validate_provider_request(
                &provider,
                &ProviderRequest::new(vec![]),
                false,
            )
            .unwrap();
        }
    }

    #[test]
    fn grok_and_groq_reasoning_efforts_are_exact() {
        let grok = OpenAIProvider::new_grok("key".to_string()).unwrap();
        assert_eq!(
            grok.capabilities("grok-4.6").reasoning.allowed_efforts,
            Some(vec![
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::Xhigh,
            ])
        );
        let groq = OpenAIProvider::new_groq("key".to_string()).unwrap();
        assert_eq!(
            groq.capabilities("openai/gpt-oss-120b")
                .reasoning
                .allowed_efforts,
            Some(vec![
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
            ])
        );
    }

    #[tokio::test]
    async fn gpt_4o_rejects_oversized_output_before_http() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        let error = provider
            .send_message(&ProviderRequest::new(vec![]).with_max_tokens(16_385))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Provider 'openai' model 'gpt-4o' supports at most 16384 output tokens, but 16385 were requested"
        );
    }

    // ── Streaming tool-call accumulation ─────────────────────────────────────

    #[test]
    fn test_accumulate_single_complete_delta() {
        // A single delta that has the full id, name, and arguments.
        let mut acc: Vec<(String, String, String)> = Vec::new();
        let delta = OpenAIToolCallDelta {
            index: Some(0),
            id: Some("call_abc".to_string()),
            tool_type: Some("function".to_string()),
            function: Some(OpenAIFunctionDelta {
                name: Some("bash".to_string()),
                arguments: Some(r#"{"command":"echo hi"}"#.to_string()),
            }),
        };
        accumulate_tool_call_delta(&mut acc, &delta);
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0].0, "call_abc");
        assert_eq!(acc[0].1, "bash");
        assert_eq!(acc[0].2, r#"{"command":"echo hi"}"#);
    }

    #[test]
    fn test_accumulate_fragmented_arguments() {
        // OpenAI often sends the arguments JSON in multiple fragments.
        let mut acc: Vec<(String, String, String)> = Vec::new();
        // First delta: has id and name
        accumulate_tool_call_delta(
            &mut acc,
            &OpenAIToolCallDelta {
                index: Some(0),
                id: Some("call_1".to_string()),
                tool_type: None,
                function: Some(OpenAIFunctionDelta {
                    name: Some("read".to_string()),
                    arguments: Some(r#"{"file_"#.to_string()),
                }),
            },
        );
        // Second delta: continues arguments
        accumulate_tool_call_delta(
            &mut acc,
            &OpenAIToolCallDelta {
                index: Some(0),
                id: None,
                tool_type: None,
                function: Some(OpenAIFunctionDelta {
                    name: None,
                    arguments: Some(r#"path":"src/main.rs"}"#.to_string()),
                }),
            },
        );
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0].0, "call_1");
        assert_eq!(acc[0].1, "read");
        assert_eq!(acc[0].2, r#"{"file_path":"src/main.rs"}"#);
    }

    #[test]
    fn test_accumulate_multiple_tool_calls() {
        // Two tool calls with different indices.
        let mut acc: Vec<(String, String, String)> = Vec::new();
        accumulate_tool_call_delta(
            &mut acc,
            &OpenAIToolCallDelta {
                index: Some(0),
                id: Some("call_0".to_string()),
                tool_type: None,
                function: Some(OpenAIFunctionDelta {
                    name: Some("bash".to_string()),
                    arguments: Some(r#"{}"#.to_string()),
                }),
            },
        );
        accumulate_tool_call_delta(
            &mut acc,
            &OpenAIToolCallDelta {
                index: Some(1),
                id: Some("call_1".to_string()),
                tool_type: None,
                function: Some(OpenAIFunctionDelta {
                    name: Some("read".to_string()),
                    arguments: Some(r#"{"file_path":"x"}"#.to_string()),
                }),
            },
        );
        assert_eq!(acc.len(), 2);
        assert_eq!(acc[0].1, "bash");
        assert_eq!(acc[1].1, "read");
    }

    #[test]
    fn test_finalize_tool_calls_parses_json() {
        let acc = vec![(
            "call_1".to_string(),
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
        )];
        let blocks = finalize_tool_calls(&acc, true).unwrap();
        assert_eq!(blocks.len(), 1);
        if let crate::claude::types::ContentBlock::ToolUse { id, name, input } = &blocks[0] {
            assert_eq!(id, "call_1");
            assert_eq!(name, "bash");
            assert_eq!(input["command"].as_str().unwrap(), "ls");
        } else {
            panic!("Expected ToolUse block");
        }
    }

    #[test]
    fn test_finalize_tool_calls_invalid_json_is_rejected() {
        let acc = vec![(
            "call_x".to_string(),
            "glob".to_string(),
            "NOT_VALID_JSON".to_string(),
        )];
        let error = finalize_tool_calls(&acc, true).unwrap_err();
        assert!(error
            .to_string()
            .contains("malformed JSON function arguments"));
    }

    #[test]
    fn test_finalize_tool_calls_empty_acc() {
        let acc: Vec<(String, String, String)> = Vec::new();
        let blocks = finalize_tool_calls(&acc, true).unwrap();
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_accumulate_default_index_zero() {
        // Delta without an explicit index should go to slot 0.
        let mut acc: Vec<(String, String, String)> = Vec::new();
        accumulate_tool_call_delta(
            &mut acc,
            &OpenAIToolCallDelta {
                index: None, // no index — should default to 0
                id: Some("call_no_idx".to_string()),
                tool_type: None,
                function: Some(OpenAIFunctionDelta {
                    name: Some("grep".to_string()),
                    arguments: Some(r#"{"pattern":"TODO"}"#.to_string()),
                }),
            },
        );
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0].0, "call_no_idx");
        assert_eq!(acc[0].1, "grep");
    }

    #[test]
    fn test_streaming_tool_calls_end_to_end_simulation() {
        // Simulate a full streaming sequence: two deltas for one tool call followed by finalize.
        // This replicates the exact pattern Grok/OpenAI uses in the wild.
        let mut acc: Vec<(String, String, String)> = Vec::new();

        // Delta 1: id + function name + start of arguments
        let delta1_json = r#"{"index":0,"id":"call_xyz","type":"function","function":{"name":"bash","arguments":"{\"comm"}}"#;
        let delta1: OpenAIToolCallDelta = serde_json::from_str(delta1_json).unwrap();
        accumulate_tool_call_delta(&mut acc, &delta1);

        // Delta 2: continuation of arguments only
        let delta2_json = r#"{"index":0,"function":{"arguments":"and\": \"echo test\"}"}}"#;
        let delta2: OpenAIToolCallDelta = serde_json::from_str(delta2_json).unwrap();
        accumulate_tool_call_delta(&mut acc, &delta2);

        // Finalize
        let blocks = finalize_tool_calls(&acc, true).unwrap();
        assert_eq!(blocks.len(), 1);
        if let crate::claude::types::ContentBlock::ToolUse { id, name, input } = &blocks[0] {
            assert_eq!(id, "call_xyz");
            assert_eq!(name, "bash");
            assert_eq!(input["command"].as_str().unwrap(), "echo test");
        } else {
            panic!("Expected ToolUse block, got {:?}", blocks[0]);
        }
    }
}
