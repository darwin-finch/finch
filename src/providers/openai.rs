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
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DECODED_IMAGE_BYTES: usize = 64 * 1024 * 1024;
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
    /// Z.ai's documented GLM-5.3-Flash Chat Completions dialect.
    ZaiGlm53Flash,
    /// Historical OpenAI-compatible shape used by xAI, Groq, Mistral, Ollama,
    /// remote Finch, custom endpoints, and pre-GPT-5.6 OpenAI models.
    CompatibleChatCompletions,
}

impl TransportRule {
    fn is_strict(self) -> bool {
        matches!(
            self,
            Self::CanonicalGpt56ChatCompletions | Self::ZaiGlm53Flash
        )
    }

    fn exposes_reasoning_content(self) -> bool {
        self == Self::ZaiGlm53Flash
    }
}

fn validate_terminal_reason(
    rule: TransportRule,
    reason: &str,
    has_tool_calls: bool,
    stream: bool,
) -> Result<()> {
    let boundary = if stream { "stream" } else { "response" };
    match reason {
        "stop" if !has_tool_calls => Ok(()),
        "tool_calls" if has_tool_calls => Ok(()),
        "stop" => anyhow::bail!("OpenAI {boundary} stopped despite containing function calls"),
        "tool_calls" => {
            anyhow::bail!("OpenAI {boundary} reported function calls without any call items")
        }
        "length" => anyhow::bail!("OpenAI {boundary} reached its output-token limit"),
        "content_filter" if rule == TransportRule::CanonicalGpt56ChatCompletions => {
            anyhow::bail!("OpenAI {boundary} was stopped by content filtering")
        }
        "sensitive" if rule == TransportRule::ZaiGlm53Flash => {
            anyhow::bail!("Z.ai {boundary} was stopped by sensitive-content filtering")
        }
        "model_context_window_exceeded" if rule == TransportRule::ZaiGlm53Flash => {
            anyhow::bail!("Z.ai {boundary} exceeded the model context window")
        }
        "network_error" if rule == TransportRule::ZaiGlm53Flash => {
            anyhow::bail!("Z.ai {boundary} ended with a provider network error")
        }
        _ => anyhow::bail!("OpenAI {boundary} returned an unknown terminal status"),
    }
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
    let validator: fn(&[u8]) -> Result<()> = match source.media_type.as_str() {
        "image/png" => validate_png,
        "image/jpeg" => validate_jpeg,
        _ => anyhow::bail!("OpenAI image media type is unsupported; expected PNG or JPEG"),
    };
    let max_base64_bytes = MAX_IMAGE_BYTES.div_ceil(3).saturating_mul(4);
    if source.data.len() > max_base64_bytes {
        anyhow::bail!("OpenAI image exceeded the 8 MB encoded-image limit");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&source.data)
        .context("OpenAI image contained invalid base64")?;
    if bytes.len() > MAX_IMAGE_BYTES {
        anyhow::bail!("OpenAI image exceeded the 8 MB encoded-image limit");
    }
    validator(&bytes)?;
    Ok(OpenAIImageUrl {
        url: format!("data:{};base64,{}", source.media_type, source.data),
    })
}

fn validate_png(bytes: &[u8]) -> Result<()> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        anyhow::bail!("OpenAI image bytes did not match the declared media type");
    }
    let mut offset = 8usize;
    let mut saw_ihdr = false;
    let mut saw_idat = false;
    while offset < bytes.len() {
        let header_end = offset.checked_add(8).context("OpenAI PNG was truncated")?;
        if header_end > bytes.len() {
            anyhow::bail!("OpenAI PNG was truncated");
        }
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let chunk_type = &bytes[offset + 4..offset + 8];
        let chunk_end = header_end
            .checked_add(length)
            .and_then(|end| end.checked_add(4))
            .context("OpenAI PNG chunk length overflowed")?;
        if chunk_end > bytes.len() {
            anyhow::bail!("OpenAI PNG was truncated");
        }
        let expected_crc =
            u32::from_be_bytes(bytes[header_end + length..chunk_end].try_into().unwrap());
        if png_crc32(&bytes[offset + 4..header_end + length]) != expected_crc {
            anyhow::bail!("OpenAI PNG failed integrity validation");
        }
        if !saw_ihdr {
            if chunk_type != b"IHDR" || length != 13 {
                anyhow::bail!("OpenAI PNG omitted a valid leading IHDR chunk");
            }
            let width = u32::from_be_bytes(bytes[header_end..header_end + 4].try_into().unwrap());
            let height =
                u32::from_be_bytes(bytes[header_end + 4..header_end + 8].try_into().unwrap());
            if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 100_000_000 {
                anyhow::bail!("OpenAI PNG dimensions were invalid or excessive");
            }
            let bit_depth = bytes[header_end + 8];
            let color_type = bytes[header_end + 9];
            let valid_depth = match color_type {
                0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
                2 | 4 | 6 => matches!(bit_depth, 8 | 16),
                3 => matches!(bit_depth, 1 | 2 | 4 | 8),
                _ => false,
            };
            if !valid_depth
                || bytes[header_end + 10] != 0
                || bytes[header_end + 11] != 0
                || bytes[header_end + 12] > 1
            {
                anyhow::bail!("OpenAI PNG contained an invalid IHDR");
            }
            saw_ihdr = true;
        } else if chunk_type == b"IHDR" {
            anyhow::bail!("OpenAI PNG contained a duplicate IHDR chunk");
        }
        if chunk_type == b"IDAT" && length > 0 {
            saw_idat = true;
        }
        if chunk_type == b"IEND" {
            if length != 0 || !saw_idat || chunk_end != bytes.len() {
                anyhow::bail!("OpenAI PNG had an invalid terminal IEND chunk");
            }
            let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
            decoder.set_limits(png::Limits {
                bytes: MAX_DECODED_IMAGE_BYTES,
            });
            let mut reader = decoder
                .read_info()
                .map_err(|_| anyhow::anyhow!("OpenAI PNG failed integrity validation"))?;
            let output_size = reader.output_buffer_size();
            if output_size > MAX_DECODED_IMAGE_BYTES {
                anyhow::bail!("OpenAI PNG decoded dimensions were excessive");
            }
            let mut output = vec![0; output_size];
            reader
                .next_frame(&mut output)
                .map_err(|_| anyhow::anyhow!("OpenAI PNG failed integrity validation"))?;
            return Ok(());
        }
        offset = chunk_end;
    }
    anyhow::bail!("OpenAI PNG was incomplete")
}

fn png_crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn validate_jpeg(bytes: &[u8]) -> Result<()> {
    if !bytes.starts_with(b"\xff\xd8") || !bytes.ends_with(b"\xff\xd9") {
        anyhow::bail!("OpenAI image bytes did not match the declared media type");
    }
    let mut offset = 2usize;
    let mut saw_frame = false;
    let mut saw_scan = false;
    'segments: while offset + 1 < bytes.len() {
        if bytes[offset] != 0xff {
            anyhow::bail!("OpenAI JPEG contained invalid marker framing");
        }
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        if offset >= bytes.len() {
            anyhow::bail!("OpenAI JPEG was truncated");
        }
        let marker = bytes[offset];
        offset += 1;
        if marker == 0xd9 {
            if saw_scan && offset == bytes.len() {
                return Ok(());
            }
            anyhow::bail!("OpenAI JPEG omitted valid terminal scan data");
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length_end = offset.checked_add(2).context("OpenAI JPEG was truncated")?;
        if length_end > bytes.len() {
            anyhow::bail!("OpenAI JPEG was truncated");
        }
        let length = u16::from_be_bytes(bytes[offset..length_end].try_into().unwrap()) as usize;
        if length < 2 {
            anyhow::bail!("OpenAI JPEG contained an invalid segment length");
        }
        let segment_end = offset
            .checked_add(length)
            .context("OpenAI JPEG segment length overflowed")?;
        if segment_end > bytes.len() {
            anyhow::bail!("OpenAI JPEG was truncated");
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            if length < 8 {
                anyhow::bail!("OpenAI JPEG contained an invalid frame header");
            }
            let height = u16::from_be_bytes(bytes[offset + 3..offset + 5].try_into().unwrap());
            let width = u16::from_be_bytes(bytes[offset + 5..offset + 7].try_into().unwrap());
            if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 100_000_000 {
                anyhow::bail!("OpenAI JPEG dimensions were invalid or excessive");
            }
            saw_frame = true;
        }
        if marker == 0xda {
            if !saw_frame {
                anyhow::bail!("OpenAI JPEG scan preceded its frame header");
            }
            let scan_start = segment_end;
            let mut scan = scan_start;
            while scan + 1 < bytes.len() {
                if bytes[scan] != 0xff {
                    scan += 1;
                    continue;
                }
                let marker_start = scan;
                while scan < bytes.len() && bytes[scan] == 0xff {
                    scan += 1;
                }
                if scan >= bytes.len() {
                    anyhow::bail!("OpenAI JPEG was incomplete");
                }
                let next = bytes[scan];
                if next == 0x00 {
                    scan += 1;
                    continue;
                }
                if (0xd0..=0xd7).contains(&next) {
                    scan += 1;
                    continue;
                }
                if marker_start == scan_start {
                    anyhow::bail!("OpenAI JPEG contained an empty scan");
                }
                saw_scan = true;
                offset = marker_start;
                continue 'segments;
            }
            anyhow::bail!("OpenAI JPEG was incomplete");
        }
        offset = segment_end;
    }
    anyhow::bail!("OpenAI JPEG was incomplete")
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
    if rule.is_strict() {
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

fn validate_strict_response_shape(value: &serde_json::Value, rule: TransportRule) -> Result<()> {
    let root = value
        .as_object()
        .context("OpenAI response was not a JSON object")?;
    let mut response_fields = vec![
        "id",
        "object",
        "created",
        "model",
        "choices",
        "usage",
        "service_tier",
        "system_fingerprint",
    ];
    if rule == TransportRule::ZaiGlm53Flash {
        response_fields.extend(["request_id", "web_search"]);
    }
    reject_unknown_keys(root, &response_fields, "response")?;
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
        let mut message_fields = vec!["role", "content", "tool_calls", "refusal", "annotations"];
        if rule.exposes_reasoning_content() {
            message_fields.push("reasoning_content");
        }
        reject_unknown_keys(message, &message_fields, "response message")?;
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

fn validate_canonical_actual_model(model: &str) -> Result<()> {
    if model.trim().is_empty() {
        anyhow::bail!("OpenAI response omitted the actual model");
    }
    crate::generators::validate_response_model(model)
        .map_err(|_| anyhow::anyhow!("OpenAI response actual model was invalid"))
}

#[derive(Default)]
struct CanonicalStreamState {
    response_id: Option<String>,
    model: Option<String>,
    terminal_reason: Option<String>,
    usage_seen: bool,
    done: bool,
    accumulated_text: String,
    tool_calls: Vec<(String, String, String)>,
}

fn reject_unknown_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    context: &str,
) -> Result<()> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        anyhow::bail!("OpenAI stream contained an unknown {} field", context);
    }
    Ok(())
}

fn validate_strict_chunk_shape(value: &serde_json::Value, rule: TransportRule) -> Result<()> {
    let root = value
        .as_object()
        .context("OpenAI stream event was not a JSON object")?;
    let mut event_fields = vec![
        "id",
        "object",
        "created",
        "model",
        "system_fingerprint",
        "service_tier",
        "choices",
        "usage",
    ];
    if rule == TransportRule::ZaiGlm53Flash {
        event_fields.extend(["request_id", "web_search"]);
    }
    reject_unknown_keys(root, &event_fields, "event")?;
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
        let mut delta_fields = vec!["role", "content", "tool_calls"];
        if rule.exposes_reasoning_content() {
            delta_fields.push("reasoning_content");
        }
        reject_unknown_keys(delta, &delta_fields, "delta")?;
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

fn strict_stream_data(
    state: &mut CanonicalStreamState,
    data: &str,
    rule: TransportRule,
) -> Result<Vec<StreamChunk>> {
    if state.done {
        anyhow::bail!("OpenAI stream sent data after its terminal marker");
    }
    let value: serde_json::Value =
        serde_json::from_str(data).context("OpenAI stream contained malformed JSON")?;
    validate_strict_chunk_shape(&value, rule)?;
    let chunk: OpenAIStreamChunk = serde_json::from_value(value)
        .context("OpenAI stream event did not match the documented schema")?;
    let object_valid = match rule {
        TransportRule::CanonicalGpt56ChatCompletions => {
            chunk.object.as_deref() == Some("chat.completion.chunk")
        }
        TransportRule::ZaiGlm53Flash => chunk
            .object
            .as_deref()
            .is_none_or(|object| object == "chat.completion.chunk"),
        TransportRule::CompatibleChatCompletions => unreachable!("strict parser rule"),
    };
    if !object_valid {
        anyhow::bail!("OpenAI stream contained an unknown event object");
    }
    if let Some(id) = &state.response_id {
        if id != &chunk.id {
            anyhow::bail!("OpenAI stream changed response ID mid-stream");
        }
    } else {
        state.response_id = Some(chunk.id.clone());
    }
    let first_model = state.model.is_none();
    if let Some(model) = &state.model {
        if model != &chunk.model {
            anyhow::bail!("OpenAI stream changed actual model mid-stream");
        }
    } else {
        if chunk.model.trim().is_empty() {
            anyhow::bail!("OpenAI stream omitted the actual model");
        }
        crate::generators::validate_response_model(&chunk.model)
            .map_err(|_| anyhow::anyhow!("OpenAI stream actual model was invalid"))?;
        state.model = Some(chunk.model.clone());
    }

    let mut output = Vec::new();
    if first_model {
        output.push(StreamChunk::ResponseMetadata {
            model: chunk.model.clone(),
        });
    }
    let usage_seen_in_chunk = chunk.usage.is_some();
    if let Some(usage) = chunk.usage {
        if !chunk.choices.is_empty() {
            anyhow::bail!("OpenAI stream attached usage to a choice chunk");
        }
        if state.terminal_reason.is_none() {
            anyhow::bail!("OpenAI stream reported usage before terminal status");
        }
        if state.usage_seen {
            anyhow::bail!("OpenAI stream reported duplicate usage");
        }
        state.usage_seen = true;
        output.push(StreamChunk::Usage {
            input_tokens: usage.prompt_tokens,
        });
    }
    if chunk.choices.is_empty() {
        if !usage_seen_in_chunk {
            anyhow::bail!("OpenAI stream chunk had neither a choice nor usage");
        }
        return Ok(output);
    }
    if chunk.choices.len() != 1 || chunk.choices[0].index != 0 {
        anyhow::bail!("OpenAI stream returned an unexpected choice set");
    }
    let choice = &chunk.choices[0];
    if state.terminal_reason.is_some() {
        if choice.finish_reason.is_some() {
            anyhow::bail!("OpenAI stream sent duplicate terminal status");
        }
        anyhow::bail!("OpenAI stream sent a choice after terminal status");
    }
    if let Some(role) = &choice.delta.role {
        if role != "assistant" {
            anyhow::bail!("OpenAI stream returned an unknown delta role");
        }
    }
    if choice.delta.role.is_none()
        && choice.delta.content.is_none()
        && choice.delta.tool_calls.is_none()
        && choice.delta.reasoning_content.is_none()
        && choice.finish_reason.is_none()
    {
        anyhow::bail!("OpenAI stream returned an empty non-terminal delta");
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
            if let Some(kind) = &delta.tool_type {
                if kind != "function" {
                    anyhow::bail!("OpenAI stream contained an unknown tool-call type");
                }
            }
            if let Some(id) = &delta.id {
                if state
                    .tool_calls
                    .iter()
                    .enumerate()
                    .any(|(other_index, other)| other_index != index && other.0 == *id)
                {
                    anyhow::bail!("OpenAI stream reused a function-call ID across indices");
                }
            }
            let call = &mut state.tool_calls[index];
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
        validate_terminal_reason(rule, reason, !state.tool_calls.is_empty(), true)?;
    }
    Ok(output)
}

fn mark_strict_done(state: &mut CanonicalStreamState, rule: TransportRule) -> Result<()> {
    if state.done {
        anyhow::bail!("OpenAI stream sent duplicate terminal marker");
    }
    if state.terminal_reason.is_none() {
        anyhow::bail!("OpenAI stream ended before terminal status");
    }
    if rule == TransportRule::CanonicalGpt56ChatCompletions && !state.usage_seen {
        anyhow::bail!("OpenAI stream ended without its requested usage chunk");
    }
    state.done = true;
    Ok(())
}

async fn publish_canonical_completion(
    state: &CanonicalStreamState,
    tx: &mpsc::Sender<Result<StreamChunk>>,
) -> Result<()> {
    let tool_blocks = finalize_tool_calls(&state.tool_calls, true)?;
    if !state.accumulated_text.is_empty() {
        tx.send(Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text {
            text: state.accumulated_text.clone(),
        })))
        .await
        .map_err(|_| anyhow::anyhow!("OpenAI stream receiver was dropped"))?;
    }
    for block in tool_blocks {
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

fn spawn_strict_stream_parser(
    response: reqwest::Response,
    rule: TransportRule,
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
                    if let Err(error) = mark_strict_done(&mut state, rule) {
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
                match strict_stream_data(&mut state, data, rule) {
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

fn parse_nonstream_tool_arguments(
    arguments: serde_json::Value,
    rule: TransportRule,
) -> Result<serde_json::Value> {
    let strict = |encoded: &str| -> Result<serde_json::Value> {
        if encoded.len() > MAX_TOOL_ARGUMENT_BYTES {
            anyhow::bail!("OpenAI function arguments exceeded the 1 MiB limit");
        }
        let value: serde_json::Value = serde_json::from_str(encoded)
            .context("OpenAI returned malformed JSON function arguments")?;
        if !value.is_object() {
            anyhow::bail!("OpenAI function arguments were not a JSON object");
        }
        Ok(value)
    };
    match rule {
        TransportRule::CanonicalGpt56ChatCompletions => {
            let encoded = arguments
                .as_str()
                .context("OpenAI function arguments were not a JSON string")?;
            strict(encoded)
        }
        TransportRule::ZaiGlm53Flash => match arguments {
            serde_json::Value::String(encoded) => strict(&encoded),
            value @ serde_json::Value::Object(_) => {
                if serde_json::to_vec(&value)?.len() > MAX_TOOL_ARGUMENT_BYTES {
                    anyhow::bail!("Z.ai function arguments exceeded the 1 MiB limit");
                }
                Ok(value)
            }
            _ => anyhow::bail!("Z.ai function arguments were not a JSON object or string"),
        },
        TransportRule::CompatibleChatCompletions => match arguments {
            serde_json::Value::String(encoded) => {
                Ok(serde_json::from_str(&encoded).unwrap_or_else(|_| serde_json::json!({})))
            }
            serde_json::Value::Object(_) => Ok(arguments),
            _ => Ok(serde_json::json!({})),
        },
    }
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
    canonical_zai_endpoint: bool,
}

impl OpenAIProvider {
    fn validate_request_payload(request: &OpenAIRequest) -> Result<()> {
        let encoded = serde_json::to_vec(request).context("Failed to serialize OpenAI request")?;
        if encoded.len() > MAX_REQUEST_BYTES {
            anyhow::bail!("OpenAI request exceeded the 32 MiB payload limit");
        }
        Ok(())
    }

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

    /// Create a Z.ai GLM-5.3-Flash provider using Z.ai's explicit dialect.
    pub fn new_zai(api_key: String) -> Result<Self> {
        Self::new(
            api_key,
            "https://api.z.ai/api/paas/v4".to_string(),
            "/chat/completions",
            "/models",
            "glm-5.3-flash".to_string(),
            "zai".to_string(),
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
        let canonical_zai_endpoint = provider_name == "zai"
            && endpoints.chat_url == "https://api.z.ai/api/paas/v4/chat/completions"
            && endpoints.models_url == "https://api.z.ai/api/paas/v4/models";

        Ok(Self {
            client,
            api_key,
            endpoints,
            default_model,
            provider_name,
            reasoning_effort: None,
            canonical_openai_endpoint,
            canonical_zai_endpoint,
        })
    }

    fn transport_rule(&self, model: &str) -> TransportRule {
        if self.canonical_openai_endpoint && matches!(model, "gpt-5.6-sol" | "gpt-5.6") {
            return TransportRule::CanonicalGpt56ChatCompletions;
        }
        if self.canonical_zai_endpoint && model == "glm-5.3-flash" {
            return TransportRule::ZaiGlm53Flash;
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
                    TransportRule::ZaiGlm53Flash | TransportRule::CompatibleChatCompletions => {
                        "system"
                    }
                }
                .to_string(),
                content: OpenAIMessageContent::Text(system.clone()),
            });
        }

        let mut outstanding_tool_ids = std::collections::HashSet::new();

        for msg in &request.messages {
            match msg.role.as_str() {
                "assistant" => {
                    if rule.is_strict() {
                        for block in &msg.content {
                            match block {
                                ContentBlock::Text { .. } => {}
                                ContentBlock::ToolUse { input, .. } => {
                                    if !input.is_object() {
                                        anyhow::bail!(
                                            "OpenAI function arguments were not a JSON object"
                                        );
                                    }
                                    let arguments = serde_json::to_string(input)
                                        .context("Failed to serialize OpenAI function arguments")?;
                                    if arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                                        anyhow::bail!(
                                            "OpenAI function arguments exceeded the 1 MiB limit"
                                        );
                                    }
                                }
                                ContentBlock::Image { .. } | ContentBlock::ToolResult { .. } => {
                                    anyhow::bail!(
                                        "OpenAI assistant message contained an unsupported content block"
                                    );
                                }
                            }
                        }
                    }
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
                    if rule.is_strict() {
                        for call in &tool_calls {
                            if call.id.is_empty() || call.function.name.is_empty() {
                                anyhow::bail!(
                                    "OpenAI function calls require non-empty IDs and names"
                                );
                            }
                            if !outstanding_tool_ids.insert(call.id.clone()) {
                                anyhow::bail!(
                                    "OpenAI request contained duplicate function call IDs"
                                );
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
                    if rule.is_strict() && msg.role != "user" {
                        anyhow::bail!("OpenAI request contained an unsupported message role");
                    }
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
                                TransportRule::CanonicalGpt56ChatCompletions
                                | TransportRule::ZaiGlm53Flash => {
                                    content_parts.push(OpenAIContentPart::ImageUrl {
                                        image_url: validate_image_source(source)?,
                                    });
                                }
                                TransportRule::CompatibleChatCompletions => {
                                    compatible_text_parts.push("[image]");
                                }
                            },
                            ContentBlock::ToolUse { .. } => {
                                if rule.is_strict() {
                                    anyhow::bail!(
                                        "OpenAI user message contained an unsupported content block"
                                    );
                                }
                            }
                        }
                    }

                    if rule.is_strict() && !content_parts.is_empty() && !tool_results.is_empty() {
                        anyhow::bail!(
                            "OpenAI user messages cannot mix tool results with user content"
                        );
                    }

                    let content = match rule {
                        TransportRule::CanonicalGpt56ChatCompletions => {
                            if content_parts.is_empty() {
                                None
                            } else {
                                Some(OpenAIMessageContent::Parts(content_parts))
                            }
                        }
                        TransportRule::ZaiGlm53Flash => {
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
                        if rule.is_strict() && !outstanding_tool_ids.remove(&tool_call_id) {
                            anyhow::bail!(
                                "OpenAI tool result references an unknown function call ID"
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
        if rule.is_strict() && !outstanding_tool_ids.is_empty() {
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
                .then_some(request.max_tokens)
                .or_else(|| (rule == TransportRule::ZaiGlm53Flash).then_some(request.max_tokens)),
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
                    include_obfuscation: Some(false),
                }),
            thinking: (rule == TransportRule::ZaiGlm53Flash).then_some(ZaiThinking {
                thinking_type: "enabled",
                clear_thinking: true,
            }),
            tool_stream: (rule == TransportRule::ZaiGlm53Flash && request.stream).then_some(true),
        };
        Self::validate_request_payload(&openai_request)?;
        Ok(openai_request)
    }

    /// Convert OpenAI response to ProviderResponse
    fn parse_response(
        &self,
        response: OpenAIResponse,
        rule: TransportRule,
    ) -> Result<ProviderResponse> {
        if rule.is_strict() {
            let object_valid = match rule {
                TransportRule::CanonicalGpt56ChatCompletions => {
                    response.object.as_deref() == Some("chat.completion")
                }
                TransportRule::ZaiGlm53Flash => response
                    .object
                    .as_deref()
                    .is_none_or(|object| object == "chat.completion"),
                TransportRule::CompatibleChatCompletions => unreachable!("strict response rule"),
            };
            if !object_valid {
                anyhow::bail!("OpenAI returned an unknown response object");
            }
            validate_canonical_actual_model(&response.model)?;
            if response.choices.len() != 1 || response.choices[0].index != 0 {
                anyhow::bail!("OpenAI returned an unexpected choice set");
            }
            if response.choices[0].message.role != "assistant" {
                anyhow::bail!("OpenAI response returned a non-assistant role");
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
                        TransportRule::CanonicalGpt56ChatCompletions
                        | TransportRule::ZaiGlm53Flash => {
                            if tool_call.id.is_empty()
                                || tool_call.function.name.is_empty()
                                || !call_ids.insert(tool_call.id.clone())
                            {
                                anyhow::bail!(
                                    "OpenAI returned an invalid or duplicate function call ID/name"
                                );
                            }
                            parse_nonstream_tool_arguments(tool_call.function.arguments, rule)?
                        }
                        TransportRule::CompatibleChatCompletions => {
                            parse_nonstream_tool_arguments(tool_call.function.arguments, rule)?
                        }
                    };
                    content.push(ContentBlock::ToolUse {
                        id: tool_call.id,
                        name: tool_call.function.name,
                        input,
                    });
                } else if rule.is_strict() {
                    anyhow::bail!("OpenAI returned an unknown tool-call type");
                }
            }
        }

        if rule.is_strict() {
            let reason = choice
                .finish_reason
                .as_deref()
                .context("OpenAI response omitted terminal status")?;
            let has_tool_calls = content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }));
            validate_terminal_reason(rule, reason, has_tool_calls, false)?;
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
            TransportRule::CanonicalGpt56ChatCompletions | TransportRule::ZaiGlm53Flash => {
                let body = read_bounded_response_body(response).await?;
                let value: serde_json::Value =
                    serde_json::from_slice(&body).context("Failed to parse OpenAI API response")?;
                validate_strict_response_shape(&value, rule)?;
                serde_json::from_value(value)
                    .context("OpenAI response did not match the documented schema")?
            }
            TransportRule::CompatibleChatCompletions => response
                .json()
                .await
                .context("Failed to parse OpenAI API response")?,
        };

        if rule.is_strict() {
            validate_canonical_actual_model(&openai_response.model)?;
        }

        tracing::debug!(
            provider = %self.provider_name,
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
                include_obfuscation: Some(false),
            });
        }
        if rule == TransportRule::ZaiGlm53Flash {
            openai_request.tool_stream = Some(true);
        }
        Self::validate_request_payload(&openai_request)?;

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

        if rule.is_strict() {
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            if !content_type.starts_with("text/event-stream") {
                anyhow::bail!("OpenAI-compatible streaming response was not text/event-stream");
            }
            return Ok(spawn_strict_stream_parser(response, rule));
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
            "openai" => self.canonical_openai_endpoint,
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
            "zai" => self.canonical_zai_endpoint,
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
                ("zai", "glm-5.3-flash") => (
                    "https://docs.z.ai/guides/vlm/glm-5.3-flash; https://docs.z.ai/api-reference/llm/chat-completion",
                    CapabilitySupport::Supported,
                    CapabilitySupport::Supported,
                    ReasoningCapability::allowed(
                        [
                            ReasoningEffort::Low,
                            ReasoningEffort::High,
                            ReasoningEffort::Max,
                        ],
                        "2026-08-27",
                        "https://docs.z.ai/guides/vlm/glm-5.3-flash",
                    )
                    .always_on(),
                    1_000_000,
                    Some(131_072),
                ),
                // Ollama and remote-daemon catalogs are deployment-specific;
                // all optional capabilities remain unknown until attested.
                _ => return ModelCapabilities::unknown(self.name(), model),
            };
        let tested_on = if self.provider_name == "zai" {
            "2026-08-27"
        } else {
            "2026-08-26"
        };
        let mut capabilities = ModelCapabilities::static_metadata(
            self.name(),
            model,
            tested_on,
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
            tested_on,
            "Finch OpenAI-compatible chat-completions adapter",
        );
        if self.provider_name == "openai" && matches!(model, "gpt-5.6-sol" | "gpt-5.6") {
            capabilities.image_input = ModelFeature::static_metadata(
                CapabilitySupport::Supported,
                "2026-08-27",
                "https://developers.openai.com/api/docs/models/gpt-5.6-sol",
            );
        }
        if self.provider_name == "zai" && model == "glm-5.3-flash" {
            capabilities.image_input = ModelFeature::static_metadata(
                CapabilitySupport::Supported,
                "2026-08-27",
                "https://docs.z.ai/guides/vlm/glm-5.3-flash",
            );
            capabilities.usage_reporting = ModelFeature::static_metadata(
                CapabilitySupport::Supported,
                "2026-08-27",
                "https://docs.z.ai/api-reference/llm/chat-completion",
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
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ZaiThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_stream: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIStreamOptions {
    include_usage: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_obfuscation: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
struct ZaiThinking {
    #[serde(rename = "type")]
    thinking_type: &'static str,
    clear_thinking: bool,
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
    #[serde(default)]
    reasoning_content: Option<String>,
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
    arguments: serde_json::Value,
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
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[cfg(test)]
fn canonical_stream_data(state: &mut CanonicalStreamState, data: &str) -> Result<Vec<StreamChunk>> {
    strict_stream_data(state, data, TransportRule::CanonicalGpt56ChatCompletions)
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

    const VALID_PNG_BASE64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    // Public progressive, multi-scan JPEG fixture from corkami/pocs
    // (SHA 359dd741bd56611e383690bb0483a38b2bfb9584).
    const VALID_PROGRESSIVE_JPEG_BASE64: &str = "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";

    // Same public fixture advanced through its second progressive scan
    // (SHA e6b9af95f5bf9d2c8f52ad16ae27c05cc726af85).
    const VALID_PROGRESSIVE_MULTI_SCAN_JPEG_BASE64: &str = "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";

    fn valid_jpeg_base64() -> String {
        base64::engine::general_purpose::STANDARD.encode([
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11,
            0x00, 0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00, 0x01, 0xff, 0xd9,
        ])
    }

    fn corrupted_png(corrupt_crc_only: bool) -> String {
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(VALID_PNG_BASE64)
            .unwrap();
        let mut offset = 8usize;
        loop {
            let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let data_start = offset + 8;
            let crc_start = data_start + length;
            if &bytes[offset + 4..offset + 8] == b"IDAT" {
                if corrupt_crc_only {
                    bytes[crc_start] ^= 1;
                } else {
                    bytes[data_start] ^= 1;
                    let crc = png_crc32(&bytes[offset + 4..crc_start]);
                    bytes[crc_start..crc_start + 4].copy_from_slice(&crc.to_be_bytes());
                }
                return base64::engine::general_purpose::STANDARD.encode(bytes);
            }
            offset = crc_start + 4;
        }
    }

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

    fn zai_test_provider(base_url: String) -> OpenAIProvider {
        let mut provider = OpenAIProvider::new_compatible(
            "zai-test-secret".to_string(),
            base_url,
            "/chat/completions",
            "/models",
            "glm-5.3-flash".to_string(),
            "zai".to_string(),
        )
        .unwrap()
        .with_reasoning_effort(ReasoningEffort::Max);
        provider.canonical_zai_endpoint = true;
        provider
    }

    fn public_canonical_test_provider(base_url: String) -> OpenAIProvider {
        let mut provider = OpenAIProvider::new_openai("test-secret".to_string())
            .unwrap()
            .with_model("gpt-5.6-sol")
            .with_reasoning_effort(ReasoningEffort::High);
        provider.endpoints.chat_url = format!("{base_url}/v1/chat/completions");
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

    async fn retrying_stalling_http_server(
        attempts: usize,
    ) -> (
        String,
        mpsc::UnboundedReceiver<usize>,
        mpsc::UnboundedReceiver<usize>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = mpsc::unbounded_channel();
        let (closed_tx, closed_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            for attempt in 0..attempts {
                let (mut socket, _) = listener.accept().await.unwrap();
                let accepted_tx = accepted_tx.clone();
                let closed_tx = closed_tx.clone();
                tokio::spawn(async move {
                    let mut request = vec![0u8; 16 * 1024];
                    let _ = socket.read(&mut request).await;
                    let _ = accepted_tx.send(attempt);
                    let mut byte = [0u8; 1];
                    while matches!(socket.read(&mut byte).await, Ok(1)) {}
                    let _ = closed_tx.send(attempt);
                });
            }
        });
        (format!("http://{}", address), accepted_rx, closed_rx)
    }

    async fn recv_without_advancing_time(
        receiver: &mut mpsc::UnboundedReceiver<usize>,
        event: &str,
    ) -> usize {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match receiver.try_recv() {
                Ok(attempt) => return attempt,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    panic!("server disconnected before reporting {event}")
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            assert!(
                std::time::Instant::now() < deadline,
                "server did not report {event} before the wall-clock deadline"
            );
            // Keeping a task runnable prevents Tokio's paused clock from
            // auto-advancing the client timeout before TCP accept/readiness.
            tokio::task::yield_now().await;
        }
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
        let jpeg = valid_jpeg_base64();
        let expected = serde_json::json!({
            "model": "gpt-5.6-sol",
            "messages": [
                {"role":"developer","content":"guard"},
                {"role":"user","content":[
                    {"type":"text","text":"inspect"},
                    {"type":"image_url","image_url":{"url":format!("data:image/png;base64,{VALID_PNG_BASE64}")}},
                    {"type":"image_url","image_url":{"url":format!("data:image/jpeg;base64,{jpeg}")}}
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
                    ContentBlock::image("image/png", VALID_PNG_BASE64),
                    ContentBlock::image("image/jpeg", jpeg),
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
                vec![ContentBlock::image("image/png", VALID_PNG_BASE64)],
            )]),
            true,
        )
        .unwrap();
        assert_eq!(validated.capabilities().model, "gpt-5.6");
    }

    #[tokio::test]
    async fn canonical_stream_and_nonstream_preserve_the_same_actual_model() {
        let actual_model = "gpt-5.6-sol-2026-08-01";
        let mut nonstream_server = mockito::Server::new_async().await;
        let nonstream = nonstream_server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id":"chatcmpl-nonstream",
                    "object":"chat.completion",
                    "model":actual_model,
                    "choices":[{
                        "index":0,
                        "message":{"role":"assistant","content":"ok"},
                        "finish_reason":"stop"
                    }]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let mut stream_server = mockito::Server::new_async().await;
        let stream = stream_server
            .mock("POST", "/v1/chat/completions")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "stream": true
            })))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(format!(
                "data: {{\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"model\":\"{actual_model}\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"model\":\"{actual_model}\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"ok\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"model\":\"{actual_model}\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: {{\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"model\":\"{actual_model}\",\"choices\":[],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}}}\n\ndata: [DONE]\n\n"
            ))
            .create_async()
            .await;
        let request = ProviderRequest::new(vec![crate::claude::Message::user("hello")])
            .with_model("gpt-5.6-sol");
        let response = canonical_test_provider(nonstream_server.url())
            .send_message_once(&request)
            .await
            .unwrap();
        let mut receiver = canonical_test_provider(stream_server.url())
            .send_message_stream_once(&request)
            .await
            .unwrap();
        let mut chunks = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            chunks.push(chunk.unwrap());
        }
        let streamed_model = chunks.iter().find_map(|chunk| match chunk {
            StreamChunk::ResponseMetadata { model } => Some(model.as_str()),
            _ => None,
        });
        assert_eq!(streamed_model, Some(response.model.as_str()));
        assert!(matches!(
            chunks.first(),
            Some(StreamChunk::ResponseMetadata { .. })
        ));
        assert_eq!(
            chunks
                .iter()
                .filter(|chunk| matches!(chunk, StreamChunk::ResponseMetadata { .. }))
                .count(),
            1
        );
        nonstream.assert_async().await;
        stream.assert_async().await;
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
                "stream_options":{"include_usage":true,"include_obfuscation":false},
                "max_completion_tokens":4096
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
        let mut actual_model = None;
        while let Some(item) = rx.recv().await {
            match item.unwrap() {
                StreamChunk::ContentBlockComplete(ContentBlock::ToolUse { id, name, input }) => {
                    calls.push((id, name, input))
                }
                StreamChunk::Usage { input_tokens } => usage = Some(input_tokens),
                StreamChunk::ResponseMetadata { model } => actual_model = Some(model),
                _ => {}
            }
        }
        assert_eq!(usage, Some(12));
        assert_eq!(actual_model.as_deref(), Some("gpt-5.6-sol-actual"));
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

    #[tokio::test(start_paused = true)]
    async fn public_validated_dispatch_enforces_request_timeout() {
        let (url, mut accepted, mut closed) = retrying_stalling_http_server(3).await;
        let mut provider = public_canonical_test_provider(url);
        provider.client = Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let request = tokio::spawn(async move {
            provider
                .send_message(
                    &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                        .with_model("gpt-5.6-sol"),
                )
                .await
        });

        for attempt in 0..3 {
            assert_eq!(
                recv_without_advancing_time(&mut accepted, "an accepted request").await,
                attempt
            );
            tokio::time::advance(Duration::from_millis(51)).await;
            assert_eq!(
                recv_without_advancing_time(&mut closed, "a released transport").await,
                attempt
            );
            if attempt < 2 {
                // Let with_retry register its backoff before advancing it.
                for _ in 0..10 {
                    tokio::task::yield_now().await;
                }
                tokio::time::advance(Duration::from_millis(1_001 * (1 << attempt))).await;
            }
        }

        let error = request.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("Failed to send request"));
    }

    #[tokio::test]
    async fn canonical_stream_post_header_timeout_errors_and_releases_transport() {
        let (url, closed) = stalling_http_server(true).await;
        let mut provider = canonical_test_provider(url);
        provider.client = Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let mut rx = provider
            .send_message_stream_once(
                &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                    .with_model("gpt-5.6-sol"),
            )
            .await
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("post-header stream timeout did not surface")
            .expect("parser ended without reporting the timeout")
            .unwrap_err();
        assert!(!error.to_string().is_empty());
        drop(rx);
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("post-header timeout did not release transport")
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
                "data: {{\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"provisional\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: {{\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}}}\n\ndata: [DONE]\n\n{}",
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
    async fn canonical_stream_rejects_incomplete_terminal_and_post_terminal_choice() {
        for body in [
            "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n",
            "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"content_filter\"}]}\n\ndata: [DONE]\n\n",
            "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
            "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
            "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_x\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        ] {
            let (complete, errors) = canonical_stream_outcome(body.to_string()).await;
            assert!(!complete);
            assert_eq!(errors.len(), 1);
        }
    }

    #[tokio::test]
    async fn canonical_stream_requires_one_terminal_usage_only_chunk() {
        for (body, expected) in [
            (
                "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[]}\n\n",
                "neither a choice nor usage",
            ),
            (
                "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                "without its requested usage",
            ),
            (
                "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
                "before terminal status",
            ),
            (
                "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
                "attached usage to a choice",
            ),
            (
                "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\ndata: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
                "duplicate usage",
            ),
        ] {
            let (complete, errors) = canonical_stream_outcome(body.to_string()).await;
            assert!(!complete);
            assert!(errors.iter().any(|error| error.contains(expected)));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn canonical_actual_model_metadata_is_bounded_redacted_and_log_safe() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&captured);
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || CapturedLogs(Arc::clone(&writer)))
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        for model in [
            format!("MODEL_SECRET_{}", "m".repeat(300)),
            "bad\nmodel".to_string(),
        ] {
            let event = serde_json::json!({
                "id":"x",
                "object":"chat.completion.chunk",
                "model":model.clone(),
                "choices":[{
                    "index":0,
                    "delta":{"role":"assistant"},
                    "finish_reason":null
                }]
            });
            let (complete, errors) = canonical_stream_outcome(format!("data: {event}\n\n")).await;
            assert!(!complete);
            assert_eq!(errors.len(), 1);
            assert!(errors[0].contains("actual model was invalid"));
            assert!(errors[0].len() < 128);
            assert!(!errors[0].contains(&model));

            let mut server = mockito::Server::new_async().await;
            server
                .mock("POST", "/v1/chat/completions")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(
                    serde_json::json!({
                        "id":"x",
                        "object":"chat.completion",
                        "model":model.clone(),
                        "choices":[{
                            "index":0,
                            "message":{"role":"assistant","content":"ok"},
                            "finish_reason":"stop"
                        }]
                    })
                    .to_string(),
                )
                .create_async()
                .await;
            let error = canonical_test_provider(server.url())
                .send_message_once(
                    &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                        .with_model("gpt-5.6-sol"),
                )
                .await
                .unwrap_err()
                .to_string();
            assert_eq!(error, "OpenAI response actual model was invalid");
            assert!(!error.contains(&model));
        }
        let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(!logs.contains("MODEL_SECRET_"));
        assert!(!logs.contains("bad\nmodel"));
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

        let mut state = CanonicalStreamState::default();
        assert!(canonical_stream_data(&mut state, r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_same","type":"function","function":{"name":"read","arguments":"{}"}},{"index":1,"id":"call_same","type":"function","function":{"name":"read","arguments":"{}"}}]},"finish_reason":null}]}"#).unwrap_err().to_string().contains("reused a function-call ID"));
    }

    #[test]
    fn canonical_images_and_tool_results_fail_closed_before_http() {
        let provider = canonical_test_provider("http://127.0.0.1:1".into());
        let progressive_single_scan = base64::engine::general_purpose::STANDARD
            .decode(VALID_PROGRESSIVE_JPEG_BASE64)
            .unwrap();
        assert!(progressive_single_scan
            .windows(2)
            .any(|marker| marker == [0xff, 0xc2]));
        validate_jpeg(&progressive_single_scan).unwrap();
        let progressive = base64::engine::general_purpose::STANDARD
            .decode(VALID_PROGRESSIVE_MULTI_SCAN_JPEG_BASE64)
            .unwrap();
        assert!(
            progressive
                .windows(2)
                .filter(|marker| *marker == [0xff, 0xda])
                .count()
                > 1
        );
        validate_jpeg(&progressive).unwrap();
        let progressive_request = ProviderRequest::new(vec![crate::claude::Message::with_content(
            "user",
            vec![ContentBlock::image(
                "image/jpeg",
                VALID_PROGRESSIVE_MULTI_SCAN_JPEG_BASE64,
            )],
        )])
        .with_model("gpt-5.6-sol");
        provider.to_openai_request(&progressive_request).unwrap();
        assert!(validate_jpeg(&progressive[..progressive.len() - 2]).is_err());
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
        for (media_type, data) in [("image/png", "iVBORw0KGgo="), ("image/jpeg", "/9j/")] {
            let truncated = ProviderRequest::new(vec![crate::claude::Message::with_content(
                "user",
                vec![ContentBlock::image(media_type, data)],
            )])
            .with_model("gpt-5.6-sol");
            assert!(provider.to_openai_request(&truncated).is_err());
        }
        for corrupt in [corrupted_png(true), corrupted_png(false)] {
            let request = ProviderRequest::new(vec![crate::claude::Message::with_content(
                "user",
                vec![ContentBlock::image("image/png", corrupt)],
            )])
            .with_model("gpt-5.6-sol");
            assert!(provider
                .to_openai_request(&request)
                .unwrap_err()
                .to_string()
                .contains("integrity validation"));
        }
        let exact_limit = ImageSource {
            source_type: "base64".into(),
            media_type: "image/png".into(),
            data: base64::engine::general_purpose::STANDARD.encode(vec![0; MAX_IMAGE_BYTES]),
        };
        assert!(!validate_image_source(&exact_limit)
            .unwrap_err()
            .to_string()
            .contains("8 MB"));
        let over_limit = ImageSource {
            source_type: "base64".into(),
            media_type: "image/png".into(),
            data: base64::engine::general_purpose::STANDARD.encode(vec![0; MAX_IMAGE_BYTES + 1]),
        };
        assert!(validate_image_source(&over_limit)
            .unwrap_err()
            .to_string()
            .contains("8 MB"));
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

    #[test]
    fn canonical_role_blocks_and_replayed_arguments_fail_closed_at_boundaries() {
        let provider = canonical_test_provider("http://127.0.0.1:1".into());
        for block in [
            ContentBlock::image("image/png", VALID_PNG_BASE64),
            ContentBlock::tool_result("call_x".into(), "result".into(), None),
        ] {
            let request = ProviderRequest::new(vec![crate::claude::Message::with_content(
                "assistant",
                vec![block],
            )])
            .with_model("gpt-5.6-sol");
            assert!(provider
                .to_openai_request(&request)
                .unwrap_err()
                .to_string()
                .contains("assistant message contained an unsupported content block"));
        }

        let user_tool_call = ProviderRequest::new(vec![crate::claude::Message::with_content(
            "user",
            vec![ContentBlock::ToolUse {
                id: "call_x".into(),
                name: "read".into(),
                input: serde_json::json!({}),
            }],
        )])
        .with_model("gpt-5.6-sol");
        assert!(provider
            .to_openai_request(&user_tool_call)
            .unwrap_err()
            .to_string()
            .contains("user message contained an unsupported content block"));

        let scalar_arguments = ProviderRequest::new(vec![crate::claude::Message::with_content(
            "assistant",
            vec![ContentBlock::ToolUse {
                id: "call_x".into(),
                name: "read".into(),
                input: serde_json::json!("scalar"),
            }],
        )])
        .with_model("gpt-5.6-sol");
        assert!(provider
            .to_openai_request(&scalar_arguments)
            .unwrap_err()
            .to_string()
            .contains("not a JSON object"));

        let exact_string_bytes = MAX_TOOL_ARGUMENT_BYTES - r#"{"data":""}"#.len();
        let exact_input = serde_json::json!({"data": "x".repeat(exact_string_bytes)});
        assert_eq!(
            serde_json::to_string(&exact_input).unwrap().len(),
            MAX_TOOL_ARGUMENT_BYTES
        );
        let matched = |input| {
            ProviderRequest::new(vec![
                crate::claude::Message::with_content(
                    "assistant",
                    vec![ContentBlock::ToolUse {
                        id: "call_x".into(),
                        name: "read".into(),
                        input,
                    }],
                ),
                crate::claude::Message::with_content(
                    "user",
                    vec![ContentBlock::tool_result(
                        "call_x".into(),
                        "ok".into(),
                        None,
                    )],
                ),
            ])
            .with_model("gpt-5.6-sol")
        };
        provider.to_openai_request(&matched(exact_input)).unwrap();
        let over_input = serde_json::json!({"data": "x".repeat(exact_string_bytes + 1)});
        assert!(provider
            .to_openai_request(&matched(over_input))
            .unwrap_err()
            .to_string()
            .contains("1 MiB limit"));

        let compatible = OpenAIProvider::new_compatible(
            "test-secret".into(),
            "http://127.0.0.1:1".into(),
            "/v1/chat/completions",
            "/v1/models",
            "compatible-model".into(),
            "compatible".into(),
        )
        .unwrap();
        let compatible_request = compatible
            .to_openai_request(
                &ProviderRequest::new(vec![crate::claude::Message::with_content(
                    "assistant",
                    vec![ContentBlock::ToolUse {
                        id: "call_x".into(),
                        name: "read".into(),
                        input: serde_json::json!("scalar"),
                    }],
                )])
                .with_model("compatible-model"),
            )
            .unwrap();
        let wire = serde_json::to_value(compatible_request).unwrap();
        assert_eq!(
            wire["messages"][0]["tool_calls"][0]["function"]["arguments"],
            "\"scalar\""
        );
    }

    #[tokio::test(start_paused = true)]
    async fn public_invalid_image_fails_before_any_http_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider =
            public_canonical_test_provider(format!("http://{}", listener.local_addr().unwrap()));
        for data in [
            "iVBORw0KGgo=".to_string(),
            corrupted_png(true),
            corrupted_png(false),
            base64::engine::general_purpose::STANDARD.encode(vec![0; MAX_IMAGE_BYTES + 1]),
        ] {
            let request = ProviderRequest::new(vec![crate::claude::Message::with_content(
                "user",
                vec![ContentBlock::image("image/png", data)],
            )])
            .with_model("gpt-5.6-sol");
            let error = provider.send_message(&request).await.unwrap_err();
            assert!(error.to_string().len() < 256);
        }
        for request in [
            ProviderRequest::new(vec![crate::claude::Message::with_content(
                "assistant",
                vec![ContentBlock::image("image/png", VALID_PNG_BASE64)],
            )])
            .with_model("gpt-5.6-sol"),
            ProviderRequest::new(vec![crate::claude::Message::with_content(
                "user",
                vec![ContentBlock::ToolUse {
                    id: "call_x".into(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                }],
            )])
            .with_model("gpt-5.6-sol"),
            ProviderRequest::new(vec![crate::claude::Message::with_content(
                "assistant",
                vec![ContentBlock::ToolUse {
                    id: "call_x".into(),
                    name: "read".into(),
                    input: serde_json::json!({
                        "data": "x".repeat(MAX_TOOL_ARGUMENT_BYTES)
                    }),
                }],
            )])
            .with_model("gpt-5.6-sol"),
        ] {
            let error = provider.send_message(&request).await.unwrap_err();
            assert!(error.to_string().len() < 256);
        }
        for blocks in [
            vec![
                ContentBlock::tool_result("call_x".into(), "ok".into(), None),
                ContentBlock::text("next"),
            ],
            vec![
                ContentBlock::text("next"),
                ContentBlock::tool_result("call_x".into(), "ok".into(), None),
            ],
        ] {
            let request = ProviderRequest::new(vec![
                crate::claude::Message::with_content(
                    "assistant",
                    vec![ContentBlock::ToolUse {
                        id: "call_x".into(),
                        name: "read".into(),
                        input: serde_json::json!({}),
                    }],
                ),
                crate::claude::Message::with_content("user", blocks),
            ])
            .with_model("gpt-5.6-sol");
            assert_eq!(
                provider
                    .send_message(&request)
                    .await
                    .unwrap_err()
                    .to_string(),
                "OpenAI user messages cannot mix tool results with user content"
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(1), listener.accept())
                .await
                .is_err(),
            "invalid image reached the HTTP transport"
        );
    }

    #[tokio::test]
    async fn canonical_request_and_response_payload_limits_hold_at_http_boundary() {
        let provider = canonical_test_provider("http://127.0.0.1:1".into());
        let empty =
            ProviderRequest::new(vec![crate::claude::Message::user("")]).with_model("gpt-5.6-sol");
        let empty_size = serde_json::to_vec(&provider.to_openai_request(&empty).unwrap())
            .unwrap()
            .len();
        let exact = ProviderRequest::new(vec![crate::claude::Message::user(
            "a".repeat(MAX_REQUEST_BYTES - empty_size),
        )])
        .with_model("gpt-5.6-sol");
        assert_eq!(
            serde_json::to_vec(&provider.to_openai_request(&exact).unwrap())
                .unwrap()
                .len(),
            MAX_REQUEST_BYTES
        );
        assert!(provider
            .send_message_stream_once(&exact)
            .await
            .unwrap_err()
            .to_string()
            .contains("payload limit"));

        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(vec![b'x'; MAX_RESPONSE_BYTES + 1])
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
        assert!(error.to_string().contains("32 MiB payload limit"));
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
        let tool_secret = "TOOL_ARGUMENT_PRIVATE_VALUE";
        let reasoning_secret = "REASONING_PRIVATE_VALUE";
        let request = ProviderRequest::new(vec![
            crate::claude::Message::with_content(
                "user",
                vec![
                    ContentBlock::text(prompt_secret),
                    ContentBlock::image("image/png", VALID_PNG_BASE64),
                ],
            ),
            crate::claude::Message::with_content(
                "assistant",
                vec![ContentBlock::ToolUse {
                    id: "call_secret".into(),
                    name: "inspect".into(),
                    input: serde_json::json!({
                        "argument": tool_secret,
                        "reasoning": reasoning_secret,
                    }),
                }],
            ),
            crate::claude::Message::with_content(
                "user",
                vec![ContentBlock::tool_result(
                    "call_secret".into(),
                    "private result".into(),
                    None,
                )],
            ),
        ])
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
            tool_secret,
            reasoning_secret,
            VALID_PNG_BASE64,
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
                r#"{"id":"x","object":"chat.completion","model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"assistant","content":"partial"},"finish_reason":"length"}]}"#,
                "output-token limit",
            ),
            (
                r#"{"id":"x","object":"chat.completion","model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"assistant","content":"partial"},"finish_reason":"content_filter"}]}"#,
                "content filtering",
            ),
            (
                r#"{"id":"x","object":"chat.completion","model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"user","content":"ok"},"finish_reason":"stop"}]}"#,
                "non-assistant role",
            ),
            (
                r#"{"id":"x","object":"chat.completion","model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"assistant","content":null},"finish_reason":"tool_calls"}]}"#,
                "without any call items",
            ),
            (
                r#"{"id":"x","object":"chat.completion","model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_x","type":"function","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"stop"}]}"#,
                "despite containing function calls",
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
    async fn canonical_nonstream_bounds_each_tool_argument() {
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::json!({
            "id":"x",
            "object":"chat.completion",
            "model":"gpt-5.6-sol",
            "choices":[{
                "index":0,
                "message":{
                    "role":"assistant",
                    "content":null,
                    "tool_calls":[{
                        "id":"call_x",
                        "type":"function",
                        "function":{
                            "name":"read",
                            "arguments":"a".repeat(MAX_TOOL_ARGUMENT_BYTES + 1)
                        }
                    }]
                },
                "finish_reason":"tool_calls"
            }]
        })
        .to_string();
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
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
        assert!(error.to_string().contains("1 MiB limit"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn canonical_malformed_fields_and_request_errors_are_bounded_and_redacted() {
        let malicious = "MALICIOUS_PRIVATE_VALUE".repeat(50_000);
        let body = format!(
            "{{\"id\":\"x\",\"object\":\"chat.completion\",\"model\":\"gpt-5.6-sol\",\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":\"ok\",\"{}\":true}},\"finish_reason\":\"stop\"}}]}}",
            malicious
        );
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(body)
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
        let error = provider
            .send_message_once(
                &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                    .with_model("gpt-5.6-sol"),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.len() < 256);
        assert!(!error.contains("MALICIOUS_PRIVATE_VALUE"));

        let huge = "REQUEST_PRIVATE_VALUE".repeat(50_000);
        let invalid_role = ProviderRequest::new(vec![crate::claude::Message::with_content(
            huge.clone(),
            vec![ContentBlock::text("x")],
        )])
        .with_model("gpt-5.6-sol");
        let invalid_mime = ProviderRequest::new(vec![crate::claude::Message::with_content(
            "user",
            vec![ContentBlock::image(huge.clone(), "AAAA")],
        )])
        .with_model("gpt-5.6-sol");
        let duplicate_ids = ProviderRequest::new(vec![crate::claude::Message::with_content(
            "assistant",
            vec![
                ContentBlock::ToolUse {
                    id: huge.clone(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::ToolUse {
                    id: huge.clone(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                },
            ],
        )])
        .with_model("gpt-5.6-sol");
        let unknown_result = ProviderRequest::new(vec![crate::claude::Message::with_content(
            "user",
            vec![ContentBlock::tool_result(huge.clone(), "x".into(), None)],
        )])
        .with_model("gpt-5.6-sol");
        for request in [invalid_role, invalid_mime, duplicate_ids, unknown_result] {
            let error = provider
                .to_openai_request(&request)
                .unwrap_err()
                .to_string();
            assert!(error.len() < 256);
            assert!(!error.contains("REQUEST_PRIVATE_VALUE"));
        }
        for response in [
            OpenAIResponse {
                id: "x".into(),
                object: Some("chat.completion".into()),
                model: "gpt-5.6-sol".into(),
                choices: vec![OpenAIChoice {
                    index: 0,
                    message: OpenAIResponseMessage {
                        role: huge.clone(),
                        content: Some("x".into()),
                        tool_calls: None,
                        reasoning_content: None,
                    },
                    finish_reason: Some("stop".into()),
                }],
                usage: None,
            },
            OpenAIResponse {
                id: "x".into(),
                object: Some("chat.completion".into()),
                model: "gpt-5.6-sol".into(),
                choices: vec![OpenAIChoice {
                    index: 0,
                    message: OpenAIResponseMessage {
                        role: "assistant".into(),
                        content: Some("x".into()),
                        tool_calls: None,
                        reasoning_content: None,
                    },
                    finish_reason: Some(huge.clone()),
                }],
                usage: None,
            },
        ] {
            let error = provider
                .parse_response(response, TransportRule::CanonicalGpt56ChatCompletions)
                .unwrap_err()
                .to_string();
            assert!(error.len() < 256);
            assert!(!error.contains("REQUEST_PRIVATE_VALUE"));
        }
        let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(!logs.contains("MALICIOUS_PRIVATE_VALUE"));
        assert!(!logs.contains("REQUEST_PRIVATE_VALUE"));
        assert!(logs.len() < 16 * 1024);
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
    async fn compatible_stream_keeps_accepting_default_obfuscation_field() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(concat!(
                "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"compatible-model\",\"obfuscation\":\"padding\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
                "data: [DONE]\n\n"
            ))
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
        let mut rx = provider
            .send_message_stream_once(&ProviderRequest::new(vec![crate::claude::Message::user(
                "hello",
            )]))
            .await
            .unwrap();
        let mut text = String::new();
        while let Some(chunk) = rx.recv().await {
            if let StreamChunk::TextDelta(delta) = chunk.unwrap() {
                text.push_str(&delta);
            }
        }
        assert_eq!(text, "ok");
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
    fn zai_glm_5_3_flash_serializes_its_exact_dialect() {
        use crate::claude::types::{ContentBlock, Message};

        let provider = zai_test_provider("https://api.z.ai/api/paas/v4".into());
        assert_eq!(
            provider.endpoints.chat_url,
            "https://api.z.ai/api/paas/v4/chat/completions"
        );
        assert_eq!(
            provider.endpoints.models_url,
            "https://api.z.ai/api/paas/v4/models"
        );
        let request = ProviderRequest::new(vec![Message::with_content(
            "user",
            vec![
                ContentBlock::text("inspect"),
                ContentBlock::image("image/png", VALID_PNG_BASE64),
            ],
        )])
        .with_model("glm-5.3-flash")
        .with_max_tokens(321)
        .with_stream(true);
        let wire = serde_json::to_value(provider.to_openai_request(&request).unwrap()).unwrap();
        assert_eq!(wire["model"], "glm-5.3-flash");
        assert_eq!(wire["max_tokens"], 321);
        assert!(wire.get("max_completion_tokens").is_none());
        assert_eq!(wire["reasoning_effort"], "max");
        assert_eq!(wire["thinking"]["type"], "enabled");
        assert_eq!(wire["thinking"]["clear_thinking"], true);
        assert_eq!(wire["tool_stream"], true);
        assert!(wire.get("stream_options").is_none());
        assert_eq!(wire["messages"][0]["role"], "user");
        assert_eq!(wire["messages"][0]["content"][0]["type"], "text");
        assert_eq!(wire["messages"][0]["content"][1]["type"], "image_url");
    }

    #[test]
    fn zai_glm_5_3_flash_reasoning_contract_is_exact_and_always_on() {
        let provider = OpenAIProvider::new_zai("key".into()).unwrap();
        let capabilities = provider.capabilities("glm-5.3-flash");
        assert_eq!(
            capabilities.reasoning.allowed_efforts,
            Some(vec![
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ])
        );
        assert!(capabilities.reasoning.always_on);
        assert_eq!(capabilities.context_window.max_tokens, Some(1_000_000));
        assert!(capabilities.image_input.is_supported());
        assert!(capabilities.usage_reporting.is_supported());
        let invalid = provider
            .clone()
            .with_reasoning_effort(ReasoningEffort::Medium);
        let error = match crate::providers::validate_provider_request(
            &invalid,
            &ProviderRequest::new(vec![]).with_model("glm-5.3-flash"),
            false,
        ) {
            Ok(_) => panic!("medium reasoning must be rejected for GLM-5.3-Flash"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("allowed efforts: low, high, max"));
    }

    #[test]
    fn zai_reasoning_stream_is_bounded_projection_not_visible_history() {
        let mut state = CanonicalStreamState::default();
        let chunks = strict_stream_data(
            &mut state,
            r#"{"id":"zai-1","object":"chat.completion.chunk","model":"glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"private scratch"},"finish_reason":null}]}"#,
            TransportRule::ZaiGlm53Flash,
        )
        .unwrap();
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::ResponseMetadata { model }] if model == "glm-5.3-flash"
        ));
        let chunks = strict_stream_data(
            &mut state,
            r#"{"id":"zai-1","object":"chat.completion.chunk","model":"glm-5.3-flash","choices":[{"index":0,"delta":{"content":"visible"},"finish_reason":"stop"}]}"#,
            TransportRule::ZaiGlm53Flash,
        )
        .unwrap();
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::TextDelta(text)] if text == "visible"
        ));
        mark_strict_done(&mut state, TransportRule::ZaiGlm53Flash).unwrap();
        assert_eq!(state.accumulated_text, "visible");
    }

    #[test]
    fn zai_documented_nonstream_shape_accepts_object_tool_arguments_without_persisting_reasoning() {
        let provider = OpenAIProvider::new_zai("key".into()).unwrap();
        let wire = serde_json::json!({
            "id": "zai-1",
            "request_id": "request-1",
            "created": 1,
            "model": "glm-5.3-flash",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "private scratch",
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {"name": "read", "arguments": {"path": "README.md"}}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5},
            "web_search": []
        });
        validate_strict_response_shape(&wire, TransportRule::ZaiGlm53Flash).unwrap();
        let response: OpenAIResponse = serde_json::from_value(wire).unwrap();
        let parsed = provider
            .parse_response(response, TransportRule::ZaiGlm53Flash)
            .unwrap();
        assert_eq!(parsed.content.len(), 1);
        assert!(matches!(
            &parsed.content[0],
            ContentBlock::ToolUse { id, name, input }
                if id == "call-1" && name == "read" && input["path"] == "README.md"
        ));
        assert!(!format!("{parsed:?}").contains("private scratch"));
    }

    #[test]
    fn zai_documented_failure_terminal_reasons_fail_closed() {
        for (reason, expected) in [
            ("sensitive", "sensitive-content"),
            ("model_context_window_exceeded", "context window"),
            ("network_error", "network error"),
        ] {
            let error =
                validate_terminal_reason(TransportRule::ZaiGlm53Flash, reason, false, false)
                    .unwrap_err();
            assert!(error.to_string().contains(expected));
        }
    }

    #[tokio::test]
    async fn zai_posts_to_its_exact_endpoint_with_bearer_auth_and_strict_dialect() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/paas/v4/chat/completions")
            .match_header("authorization", "Bearer zai-test-secret")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model": "glm-5.3-flash",
                "messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}],
                "max_tokens": 77,
                "reasoning_effort": "max",
                "thinking": {"type": "enabled", "clear_thinking": true}
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": "zai-response-1",
                    "request_id": "request-1",
                    "created": 1,
                    "model": "glm-5.3-flash",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "hello back",
                            "reasoning_content": "private scratch"
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 2, "total_tokens": 4}
                })
                .to_string(),
            )
            .create_async()
            .await;
        let provider = zai_test_provider(format!("{}/api/paas/v4", server.url()));
        let response = provider
            .send_message_once(
                &ProviderRequest::new(vec![crate::claude::Message::user("hello")])
                    .with_model("glm-5.3-flash")
                    .with_max_tokens(77),
            )
            .await
            .unwrap();
        assert_eq!(response.provider, "zai");
        assert_eq!(response.model, "glm-5.3-flash");
        assert_eq!(response.text(), "hello back");
        assert!(!format!("{response:?}").contains("private scratch"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn zai_stream_uses_strict_parser_and_discards_reasoning_projection() {
        let mut server = mockito::Server::new_async().await;
        let events = [
            serde_json::json!({
                "id": "zai-stream-1", "request_id": "request-1", "model": "glm-5.3-flash",
                "choices": [{"index": 0, "delta": {"role": "assistant", "reasoning_content": "private scratch"}, "finish_reason": null}]
            }),
            serde_json::json!({
                "id": "zai-stream-1", "request_id": "request-1", "model": "glm-5.3-flash",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0, "id": "call-1", "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\""}
                }]}, "finish_reason": null}]
            }),
            serde_json::json!({
                "id": "zai-stream-1", "request_id": "request-1", "model": "glm-5.3-flash",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0, "function": {"arguments": "README.md\"}"}
                }]}, "finish_reason": null}]
            }),
            serde_json::json!({
                "id": "zai-stream-1", "request_id": "request-1", "model": "glm-5.3-flash",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
            }),
        ];
        let body = events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>()
            + "data: [DONE]\n\n";
        let mock = server
            .mock("POST", "/api/paas/v4/chat/completions")
            .match_header("authorization", "Bearer zai-test-secret")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model": "glm-5.3-flash",
                "stream": true,
                "tool_stream": true,
                "thinking": {"type": "enabled", "clear_thinking": true}
            })))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;
        let provider = zai_test_provider(format!("{}/api/paas/v4", server.url()));
        let mut receiver = provider
            .send_message_stream_once(
                &ProviderRequest::new(vec![crate::claude::Message::user("read it")])
                    .with_model("glm-5.3-flash"),
            )
            .await
            .unwrap();
        let mut chunks = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            chunks.push(chunk.unwrap());
        }
        assert!(chunks
            .iter()
            .all(|chunk| !format!("{chunk:?}").contains("private scratch")));
        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            StreamChunk::ContentBlockComplete(ContentBlock::ToolUse { id, name, input })
                if id == "call-1" && name == "read" && input["path"] == "README.md"
        )));
        mock.assert_async().await;
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
