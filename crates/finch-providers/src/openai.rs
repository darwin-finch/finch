// OpenAI API provider implementation
//
// This provider works for both OpenAI (GPT-4, etc.) and Grok (X.AI)
// since they use compatible API formats.

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use futures::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use super::endpoints::{resolve_endpoint, ProviderEndpoints};
use super::types::{
    CapabilityProvenance, CapabilitySupport, EventProvenance, ModelCapabilities, ModelFeature,
    ProviderRequest, ProviderResponse, StreamChunk, WireProtocol,
};
use super::{LlmProvider, ProviderBackend, ReasoningCapability, ValidatedProviderRequest};
use crate::retry::{with_retry, NonRetriableError};
#[cfg(test)]
use crate::tool_bindings::compile_from_definitions;
use crate::tool_bindings::ToolBindingTable;
use crate::ReasoningEffort;
use crate::{ContentBlock, ImageSource};

const REQUEST_TIMEOUT_SECS: u64 = 60;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DECODED_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
const MAX_SSE_TOTAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;

fn prompt_cache_key(model: &str, system: Option<&str>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"finch-openai-prompt-cache-v1\0");
    digest.update(model.as_bytes());
    digest.update(b"\0");
    if let Some(system) = system {
        digest.update(system.as_bytes());
    }
    format!("finch-{:x}", digest.finalize())
}

#[cfg(test)]
fn openai_bindings(
    provider: &str,
    request: &ProviderRequest,
    model: &str,
) -> Result<ToolBindingTable> {
    compile_from_definitions(
        WireProtocol::OpenAiChatCompletions,
        provider,
        model,
        request.tools.as_deref().unwrap_or_default(),
        request.tool_policy(),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}

fn decode_openai_tool_name(
    bindings: &ToolBindingTable,
    wire_name: &str,
    redact_wire_identity: bool,
) -> Result<String> {
    let binding = bindings
        .decode_wire_call(wire_name, None)
        .map_err(|error| {
            if redact_wire_identity {
                anyhow::anyhow!(
                "OpenAI-compatible response called a tool that was not advertised in this request"
            )
            } else {
                anyhow::anyhow!("{error}")
            }
        })?;
    Ok(binding.semantic.clone())
}

fn encode_openai_tool_name(bindings: &ToolBindingTable, semantic: &str) -> Result<String> {
    Ok(bindings
        .encode_semantic(semantic)
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .wire
        .name
        .clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportRule {
    /// Current official OpenAI Chat Completions contract for GPT-5.6.
    CanonicalGpt56ChatCompletions,
    /// Documented Meta Model API Chat Completions contract for Muse Spark.
    MetaModelApiChatCompletions,
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

/// Reuse the canonical OpenAI image integrity boundary for Responses-family
/// transports without exposing the Platform wire type.
pub(crate) fn validated_image_data_url(source: &ImageSource) -> Result<String> {
    Ok(validate_image_source(source)?.url)
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
    redact_body: bool,
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
    if redact_body {
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

fn validate_canonical_response_shape(value: &serde_json::Value, rule: TransportRule) -> Result<()> {
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
        let allowed_message_fields = match rule {
            TransportRule::MetaModelApiChatCompletions => &[
                "role",
                "content",
                "tool_calls",
                "refusal",
                "annotations",
                "reasoning_content",
            ][..],
            TransportRule::CanonicalGpt56ChatCompletions
            | TransportRule::CompatibleChatCompletions => {
                &["role", "content", "tool_calls", "refusal", "annotations"][..]
            }
        };
        reject_unknown_keys(message, allowed_message_fields, "response message")?;
        if rule == TransportRule::MetaModelApiChatCompletions {
            validate_reasoning_content_type(
                message.get("reasoning_content"),
                "OpenAI response did not match the documented schema",
            )?;
        }
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

fn validate_configured_response_types(value: &serde_json::Value) -> Result<()> {
    let root = value
        .as_object()
        .context("OpenAI-compatible response was not a JSON object")?;
    require_string(root, "id", "response")?;
    require_string(root, "object", "response")?;
    require_string(root, "model", "response")?;
    optional_unsigned(root, "created", "response")?;
    optional_string_or_null(root, "service_tier", "response")?;
    optional_string_or_null(root, "system_fingerprint", "response")?;
    validate_usage_object(root.get("usage"), "response usage")?;
    let choices = root
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .context("OpenAI-compatible response omitted a valid choices array")?;
    for choice in choices {
        let choice = choice
            .as_object()
            .context("OpenAI-compatible response choice was not an object")?;
        require_unsigned(choice, "index", "response choice")?;
        require_string_or_null(choice, "finish_reason", "response choice")?;
        if choice.get("logprobs").is_some_and(|value| !value.is_null()) {
            anyhow::bail!("OpenAI-compatible response contained unsupported log probabilities");
        }
        let message = choice
            .get("message")
            .and_then(serde_json::Value::as_object)
            .context("OpenAI-compatible response choice omitted a valid message")?;
        require_string(message, "role", "response message")?;
        require_string_or_null(message, "content", "response message")?;
        if message.get("refusal").is_some_and(|value| !value.is_null()) {
            anyhow::bail!("OpenAI-compatible response contained an unsupported refusal item");
        }
        if message
            .get("annotations")
            .is_some_and(|value| value.as_array().is_none_or(|items| !items.is_empty()))
        {
            anyhow::bail!("OpenAI-compatible response contained unsupported annotations");
        }
    }
    Ok(())
}

fn validate_configured_chunk_types(value: &serde_json::Value) -> Result<()> {
    let root = value
        .as_object()
        .context("OpenAI-compatible stream event was not a JSON object")?;
    require_string(root, "id", "stream event")?;
    require_string(root, "object", "stream event")?;
    require_string(root, "model", "stream event")?;
    optional_unsigned(root, "created", "stream event")?;
    optional_string_or_null(root, "service_tier", "stream event")?;
    optional_string_or_null(root, "system_fingerprint", "stream event")?;
    validate_usage_object(root.get("usage"), "stream usage")?;
    let choices = root
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .context("OpenAI-compatible stream event omitted a valid choices array")?;
    for choice in choices {
        let choice = choice
            .as_object()
            .context("OpenAI-compatible stream choice was not an object")?;
        require_unsigned(choice, "index", "stream choice")?;
        require_string_or_null(choice, "finish_reason", "stream choice")?;
        if choice.get("logprobs").is_some_and(|value| !value.is_null()) {
            anyhow::bail!("OpenAI-compatible stream contained unsupported log probabilities");
        }
        let delta = choice
            .get("delta")
            .and_then(serde_json::Value::as_object)
            .context("OpenAI-compatible stream choice omitted a valid delta object")?;
        optional_string_or_null(delta, "role", "stream delta")?;
        optional_string_or_null(delta, "content", "stream delta")?;
        if delta
            .get("tool_calls")
            .is_some_and(|value| value.as_array().is_none())
        {
            anyhow::bail!("OpenAI-compatible stream tool_calls was not an array");
        }
        if let Some(tool_calls) = delta
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
        {
            for tool_call in tool_calls {
                let tool_call = tool_call
                    .as_object()
                    .context("OpenAI-compatible stream tool call was not an object")?;
                require_unsigned(tool_call, "index", "stream tool call")?;
                optional_string_or_null(tool_call, "id", "stream tool call")?;
                optional_string_or_null(tool_call, "type", "stream tool call")?;
                let function = tool_call.get("function");
                if function.is_none()
                    && tool_call.get("id").is_none()
                    && tool_call.get("type").is_none()
                {
                    anyhow::bail!("OpenAI-compatible stream returned an empty tool-call delta");
                }
                if let Some(function) = function {
                    let function = function
                        .as_object()
                        .context("OpenAI-compatible stream function delta was not an object")?;
                    optional_string_or_null(function, "name", "stream function delta")?;
                    optional_string_or_null(function, "arguments", "stream function delta")?;
                    if function.is_empty() {
                        anyhow::bail!(
                            "OpenAI-compatible stream returned an empty function-call delta"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

fn require_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<()> {
    if object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .is_none()
    {
        anyhow::bail!("OpenAI-compatible {context} omitted a valid {field} field");
    }
    Ok(())
}

fn require_string_or_null(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<()> {
    let Some(value) = object.get(field) else {
        anyhow::bail!("OpenAI-compatible {context} omitted the {field} field");
    };
    if !value.is_null() && !value.is_string() {
        anyhow::bail!("OpenAI-compatible {context} had an invalid {field} field");
    }
    Ok(())
}

fn optional_string_or_null(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<()> {
    if object
        .get(field)
        .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        anyhow::bail!("OpenAI-compatible {context} had an invalid {field} field");
    }
    Ok(())
}

fn require_unsigned(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<()> {
    if object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .is_none()
    {
        anyhow::bail!("OpenAI-compatible {context} omitted a valid {field} field");
    }
    Ok(())
}

fn optional_unsigned(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<()> {
    if object
        .get(field)
        .is_some_and(|value| value.as_u64().is_none())
    {
        anyhow::bail!("OpenAI-compatible {context} had an invalid {field} field");
    }
    Ok(())
}

fn validate_usage_object(value: Option<&serde_json::Value>, context: &str) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let usage = value
        .as_object()
        .with_context(|| format!("OpenAI-compatible {context} was not an object"))?;
    require_unsigned(usage, "prompt_tokens", context)?;
    require_unsigned(usage, "completion_tokens", context)?;
    require_unsigned(usage, "total_tokens", context)
}

fn validate_canonical_actual_model(model: &str) -> Result<()> {
    if model.trim().is_empty() {
        anyhow::bail!("OpenAI response omitted the actual model");
    }
    crate::validate_response_model(model)
        .map_err(|_| anyhow::anyhow!("OpenAI response actual model was invalid"))
}

struct CanonicalStreamState {
    rule: TransportRule,
    provider: String,
    response_id: Option<String>,
    model: Option<String>,
    terminal_reason: Option<String>,
    usage_seen: bool,
    done: bool,
    accumulated_text: String,
    tool_calls: Vec<(String, String, String)>,
    tool_delta_emitted: Vec<bool>,
    sequence: u64,
    bindings: Arc<ToolBindingTable>,
    redact_wire_identities: bool,
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

fn validate_reasoning_content_type(value: Option<&serde_json::Value>, error: &str) -> Result<()> {
    if value.is_some_and(|value| !value.is_null() && !value.is_string()) {
        anyhow::bail!("{error}");
    }
    Ok(())
}

fn validate_canonical_chunk_shape(value: &serde_json::Value, rule: TransportRule) -> Result<()> {
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
        let allowed_delta_fields = match rule {
            TransportRule::MetaModelApiChatCompletions => {
                &["role", "content", "reasoning_content", "tool_calls"][..]
            }
            TransportRule::CanonicalGpt56ChatCompletions
            | TransportRule::CompatibleChatCompletions => &["role", "content", "tool_calls"][..],
        };
        reject_unknown_keys(delta, allowed_delta_fields, "delta")?;
        if rule == TransportRule::MetaModelApiChatCompletions {
            validate_reasoning_content_type(
                delta.get("reasoning_content"),
                "OpenAI stream event did not match the documented schema",
            )?;
        }
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
    validate_canonical_chunk_shape(&value, state.rule)?;
    if state.rule == TransportRule::CompatibleChatCompletions {
        validate_configured_chunk_types(&value)?;
    }
    let has_meta_reasoning_field = state.rule == TransportRule::MetaModelApiChatCompletions
        && value
            .pointer("/choices/0/delta")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|delta| delta.contains_key("reasoning_content"));
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
    let first_model = state.model.is_none();
    if let Some(model) = &state.model {
        if model != &chunk.model {
            anyhow::bail!("OpenAI stream changed actual model mid-stream");
        }
    } else {
        if chunk.model.trim().is_empty() {
            anyhow::bail!("OpenAI stream omitted the actual model");
        }
        crate::validate_response_model(&chunk.model)
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
            output_tokens: usage.completion_tokens,
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
        && choice.delta.reasoning_content.is_none()
        && choice.delta.tool_calls.is_none()
        && choice.finish_reason.is_none()
        && !has_meta_reasoning_field
    {
        anyhow::bail!("OpenAI stream returned an empty non-terminal delta");
    }
    if state.rule == TransportRule::CompatibleChatCompletions
        && choice.finish_reason.is_none()
        && choice.delta.role.as_deref().is_none_or(str::is_empty)
        && choice.delta.content.as_deref().is_none_or(str::is_empty)
        && choice
            .delta
            .tool_calls
            .as_ref()
            .is_none_or(|calls| calls.is_empty())
    {
        anyhow::bail!("OpenAI-compatible stream returned a sparse non-terminal delta");
    }
    if let Some(reasoning) = choice
        .delta
        .reasoning_content
        .as_ref()
        .filter(|reasoning| !reasoning.is_empty())
    {
        // A reasoning delta is already inside the strict 1 MiB SSE-line and
        // 4 MiB whole-stream bounds. Keep the direct check at the semantic
        // boundary too so future framing changes cannot silently unbound it.
        if reasoning.len() > MAX_SSE_LINE_BYTES {
            anyhow::bail!("OpenAI reasoning delta exceeded the 1 MiB limit");
        }
        state.sequence += 1;
        output.push(StreamChunk::ThinkingDelta {
            text: reasoning.clone(),
            provenance: EventProvenance {
                provider: state.provider.clone(),
                model: state.model.clone().unwrap_or_default(),
                event: "reasoning".to_string(),
                sequence: state.sequence,
                opaque_replay: None,
            },
        });
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
                state.tool_delta_emitted.push(false);
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
            {
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
            let (call_id, call_name, accumulated_args) = state.tool_calls[index].clone();
            if !call_id.is_empty() {
                let first_emission = !state.tool_delta_emitted[index];
                state.tool_delta_emitted[index] = true;
                let arguments_delta = if first_emission {
                    accumulated_args
                } else {
                    delta
                        .function
                        .as_ref()
                        .and_then(|function| function.arguments.clone())
                        .unwrap_or_default()
                };
                let name = if call_name.is_empty() {
                    None
                } else {
                    Some(decode_openai_tool_name(
                        &state.bindings,
                        &call_name,
                        state.redact_wire_identities,
                    )?)
                };
                state.sequence += 1;
                let sequence = state.sequence;
                output.push(StreamChunk::ToolCallDelta {
                    id: call_id,
                    name,
                    arguments_delta,
                    provenance: tool_provenance(state, sequence),
                });
            }
        }
    }
    if let Some(reason) = &choice.finish_reason {
        if state.terminal_reason.replace(reason.clone()).is_some() {
            anyhow::bail!("OpenAI stream sent duplicate terminal status");
        }
        match reason.as_str() {
            "stop" if state.tool_calls.is_empty() => {}
            "tool_calls" if !state.tool_calls.is_empty() => {}
            "stop" => anyhow::bail!("OpenAI stream stopped despite containing function calls"),
            "tool_calls" => {
                anyhow::bail!("OpenAI stream reported function calls without any call items")
            }
            "length" => anyhow::bail!("OpenAI stream reached its output-token limit"),
            "content_filter" => anyhow::bail!("OpenAI stream was stopped by content filtering"),
            _ => anyhow::bail!("OpenAI stream returned an unknown terminal status"),
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
    if matches!(
        state.rule,
        TransportRule::CanonicalGpt56ChatCompletions | TransportRule::MetaModelApiChatCompletions
    ) && !state.usage_seen
    {
        anyhow::bail!("OpenAI stream ended without its requested usage chunk");
    }
    state.done = true;
    Ok(())
}

async fn publish_canonical_completion(
    state: &CanonicalStreamState,
    tx: &mpsc::Sender<Result<StreamChunk>>,
) -> Result<()> {
    let tool_blocks = finalize_tool_calls(
        &state.tool_calls,
        true,
        &state.bindings,
        state.redact_wire_identities,
    )?;
    if !state.accumulated_text.is_empty() {
        tx.send(Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text {
            text: state.accumulated_text.clone(),
        })))
        .await
        .map_err(|_| anyhow::anyhow!("OpenAI stream receiver was dropped"))?;
    }
    for block in tool_blocks {
        if let ContentBlock::ToolUse { id, name, input } = &block {
            tx.send(Ok(StreamChunk::ToolCallComplete {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                provenance: tool_provenance(state, state.sequence.saturating_add(1)),
            }))
            .await
            .map_err(|_| anyhow::anyhow!("OpenAI stream receiver was dropped"))?;
        }
        tx.send(Ok(StreamChunk::ContentBlockComplete(block)))
            .await
            .map_err(|_| anyhow::anyhow!("OpenAI stream receiver was dropped"))?;
    }
    Ok(())
}

fn tool_provenance(state: &CanonicalStreamState, sequence: u64) -> EventProvenance {
    EventProvenance {
        provider: state.provider.clone(),
        model: state.model.clone().unwrap_or_default(),
        event: "tool_call".to_string(),
        sequence,
        opaque_replay: None,
    }
}

fn sse_line_prefix_exceeds_limit(buffer: &[u8]) -> bool {
    match buffer.iter().position(|byte| *byte == b'\n') {
        Some(position) => position.saturating_add(1) > MAX_SSE_LINE_BYTES,
        None => buffer.len() > MAX_SSE_LINE_BYTES,
    }
}

fn spawn_canonical_stream_parser(
    response: reqwest::Response,
    rule: TransportRule,
    provider: String,
    bindings: Arc<ToolBindingTable>,
    redact_wire_identities: bool,
) -> mpsc::Receiver<Result<StreamChunk>> {
    let (tx, rx) = mpsc::channel(100);
    tokio::spawn(async move {
        let mut stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut event_data: Option<String> = None;
        let mut total = 0usize;
        let mut state = CanonicalStreamState {
            rule,
            provider,
            response_id: None,
            model: None,
            terminal_reason: None,
            usage_seen: false,
            done: false,
            accumulated_text: String::new(),
            tool_calls: Vec::new(),
            tool_delta_emitted: Vec::new(),
            sequence: 0,
            bindings,
            redact_wire_identities,
        };
        loop {
            let next = tokio::select! {
                biased;
                _ = tx.closed() => return,
                next = stream.next() => next,
            };
            let Some(next) = next else {
                if event_data.is_some() || buffer.iter().any(|byte| !byte.is_ascii_whitespace()) {
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
                if tx.is_closed() {
                    return;
                }
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
                    let Some(data) = event_data.take() else {
                        continue;
                    };
                    if data == "[DONE]" {
                        if let Err(error) = mark_canonical_done(&mut state) {
                            if !tx.is_closed() {
                                let _ = tx.send(Err(error)).await;
                            }
                            return;
                        }
                    } else {
                        match canonical_stream_data(&mut state, &data) {
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
                let event = event_data.get_or_insert_with(String::new);
                if !event.is_empty() {
                    event.push('\n');
                }
                event.push_str(data);
                if event.len() > MAX_SSE_LINE_BYTES {
                    let _ = tx
                        .send(Err(anyhow::anyhow!(
                            "OpenAI SSE event exceeded the 1 MiB limit"
                        )))
                        .await;
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
/// Malformed JSON never becomes `{}`. Strict mode fails the stream;
/// compatible mode skips the block so ToolLoop can fail closed from deltas.
fn finalize_tool_calls(
    acc: &[(String, String, String)],
    strict: bool,
    bindings: &ToolBindingTable,
    redact_wire_identities: bool,
) -> Result<Vec<ContentBlock>> {
    let mut blocks = Vec::new();
    for (id, name, args_str) in acc
        .iter()
        .filter(|(id, name, _)| !id.is_empty() || !name.is_empty())
    {
        if strict && (id.is_empty() || name.is_empty()) {
            anyhow::bail!("OpenAI stream ended with an incomplete function call");
        }
        if strict && args_str.len() > MAX_TOOL_ARGUMENT_BYTES {
            anyhow::bail!("OpenAI function arguments exceeded the 1 MiB limit");
        }
        let input = match serde_json::from_str::<serde_json::Value>(args_str) {
            Ok(value) if value.is_object() => value,
            Ok(_) if strict => {
                anyhow::bail!("OpenAI function arguments were not a JSON object");
            }
            Err(_) if strict => {
                anyhow::bail!("OpenAI returned malformed JSON function arguments");
            }
            Ok(_) | Err(_) => continue,
        };
        let name = decode_openai_tool_name(bindings, name, redact_wire_identities)?;
        blocks.push(ContentBlock::ToolUse {
            id: id.clone(),
            name,
            input,
        });
    }
    Ok(blocks)
}

// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthHeader {
    Bearer,
    Named(&'static str),
}

/// OpenAI API provider
///
/// Supports both OpenAI and Grok APIs (they use the same format).
#[derive(Clone)]
pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    endpoints: ProviderEndpoints,
    default_model: String,
    /// Display/identity name — used for `name()`, logging, and the
    /// canonical-endpoint/static-table lookups below, all of which are
    /// about *which* provider this is. Kept as a plain field, separate from
    /// `profile`, because those concerns are orthogonal to the capability-
    /// attestation *strategy* `profile` selects: two providers can share a
    /// strategy (openai/grok/mistral/groq all use `ProviderProfile::Static`)
    /// while needing distinct names, and `new_compatible`/
    /// `new_compatible_named_header` let a caller supply an arbitrary name
    /// with no attestation story of its own.
    provider_name: String,
    reasoning_effort: Option<ReasoningEffort>,
    canonical_openai_endpoint: bool,
    canonical_meta_endpoint: bool,
    auth_header: AuthHeader,
    /// How this instance's model capabilities are attested. See
    /// [`ProviderProfile`].
    profile: ProviderProfile,
    compatible_tool_choice_auto: bool,
    compatible_strict_tool_schemas: Option<bool>,
}

/// Strategy this provider instance uses to answer `capabilities()` /
/// `refresh_capabilities()`. Each `new_*` constructor picks the variant
/// matching its actual capability story; the two methods below `match
/// &self.profile` exhaustively (no `_` wildcard arm), so adding a variant
/// forces every call site to decide what it means there instead of quietly
/// falling through to `ModelCapabilities::unknown(...)`.
///
/// Crate-private to this module, not to be confused with the unrelated
/// `finch::providers::ProviderProfile` (a configured provider handle plus
/// its selector name, in the outer `finch` crate's `src/providers/factory.rs`).
#[derive(Clone)]
enum ProviderProfile {
    /// openai/grok/mistral/groq: a static, dated per-model capability table
    /// declared inline in `capabilities()`, gated on the provider's
    /// configured endpoints still being the exact canonical URL for that
    /// provider (a custom endpoint falls back to `Unknown`).
    Static,
    /// Ollama: capabilities are attested live against `/api/show` and
    /// cached per model; an unattested model stays `Unknown` (fail closed)
    /// rather than assuming support.
    Ollama {
        /// The live Ollama-native `/api/show` endpoint (distinct from the
        /// OpenAI-compatible `/v1/...` surface used for chat) that
        /// `capabilities()` attests model features from.
        capability_endpoint: String,
        /// Per-model live capability attestations already fetched from
        /// Ollama. Populated by `refresh_capabilities`, read synchronously
        /// by `capabilities()`. Shared across clones of the same provider
        /// instance so a `with_model`/`with_reasoning_effort` clone reuses
        /// the cache.
        capabilities: Arc<std::sync::Mutex<HashMap<String, OllamaLiveCapabilities>>>,
    },
    /// A remote Finch daemon's OpenAI-compatible endpoint: deployment-
    /// specific, with no attestation path at all, so capabilities are
    /// always `Unknown`.
    RemoteDaemon,
    /// Operator-attested capabilities for one exact generic connection and
    /// model. These never inherit a first-party provider identity.
    Configured(ModelCapabilities),
}

/// One model's live capability attestation, fetched from Ollama's own
/// `/api/show` endpoint rather than assumed from a static allowlist.
#[derive(Debug, Clone)]
struct OllamaLiveCapabilities {
    /// Exact strings Ollama reported, e.g. `["completion", "tools"]`. This is
    /// treated as a complete listing: a feature absent from it is reported
    /// `Unsupported`, not `Unknown` — Ollama itself is the authority here.
    capabilities: Vec<String>,
    fetched_at: DateTime<Utc>,
}

/// Timeout for the capability-attestation call, kept short and independent
/// of the main chat client's timeout so a slow or hung Ollama daemon cannot
/// stall every query behind a 60s wait before falling back to `Unknown`.
const OLLAMA_CAPABILITY_CHECK_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Deserialize)]
struct OllamaShowResponse {
    #[serde(default)]
    capabilities: Vec<String>,
}

/// Fetch one model's self-reported capabilities from Ollama's native
/// `/api/show` endpoint (distinct from the OpenAI-compatible surface Finch
/// uses for chat). Any transport, status, or decode failure is returned as
/// an `Err` so the caller fails closed instead of assuming support.
async fn fetch_ollama_capabilities(
    client: &Client,
    endpoint: &str,
    model: &str,
) -> Result<Vec<String>> {
    let response = client
        .post(endpoint)
        .timeout(Duration::from_secs(OLLAMA_CAPABILITY_CHECK_TIMEOUT_SECS))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .context("failed to reach Ollama /api/show for capability attestation")?;
    if !response.status().is_success() {
        anyhow::bail!(
            "Ollama /api/show returned status {} for model '{}'",
            response.status(),
            model
        );
    }
    let body: OllamaShowResponse = response
        .json()
        .await
        .context("Ollama /api/show returned a response Finch could not parse")?;
    Ok(body.capabilities)
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
    #[cfg(test)]
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
    #[cfg(test)]
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
    ///
    /// Capabilities are not assumed from a static allowlist: Finch attests
    /// them live from Ollama's own `/api/show` endpoint the first time each
    /// model is resolved (see `refresh_capabilities`), and fails closed
    /// (`Unknown`) until that attestation succeeds.
    pub fn new_ollama(base_url: String, model: String) -> Result<Self> {
        let mut provider = Self::new(
            "ollama".to_string(), // Ollama ignores the Authorization header
            base_url.clone(),
            "/v1/chat/completions",
            "/v1/models",
            model,
            "ollama".to_string(),
        )?;
        provider.profile = ProviderProfile::Ollama {
            capability_endpoint: resolve_endpoint(&base_url, "/api/show"),
            capabilities: Arc::new(std::sync::Mutex::new(HashMap::new())),
        };
        Ok(provider)
    }

    /// Create a provider that talks to a remote finch daemon's OpenAI-compatible endpoint.
    ///
    /// The daemon exposes `/v1/chat/completions` at `address`.
    pub fn new_remote_daemon(address: String) -> Result<Self> {
        let mut provider = Self::new(
            String::new(), // no API key for the local/remote daemon
            address,
            "/v1/chat/completions",
            "/v1/models",
            "default".to_string(),
            "remote_daemon".to_string(),
        )?;
        provider.profile = ProviderProfile::RemoteDaemon;
        Ok(provider)
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
        let canonical_meta_endpoint = provider_name == "meta_model_api"
            && endpoints.chat_url == "https://api.meta.ai/v1/chat/completions"
            && endpoints.models_url == "https://api.meta.ai/v1/models";

        Ok(Self {
            client,
            api_key,
            endpoints,
            default_model,
            provider_name,
            reasoning_effort: None,
            canonical_openai_endpoint,
            canonical_meta_endpoint,
            auth_header: AuthHeader::Bearer,
            profile: ProviderProfile::Static,
            compatible_tool_choice_auto: false,
            compatible_strict_tool_schemas: None,
        })
    }

    /// Create a generic OpenAI Chat Completions provider whose capabilities
    /// and request compatibility fields come from the exact named profile.
    #[allow(clippy::too_many_arguments)]
    pub fn new_configured_compatible(
        api_key: String,
        base_url: String,
        chat_path: impl AsRef<str>,
        models_path: impl AsRef<str>,
        model: String,
        provider_name: String,
        capabilities: ModelCapabilities,
        tool_choice_auto: bool,
        strict_tool_schemas: Option<bool>,
    ) -> Result<Self> {
        if capabilities.provider != provider_name || capabilities.model != model {
            anyhow::bail!("Configured compatible capability identity did not match provider/model");
        }
        let mut provider = Self::new(
            api_key,
            base_url,
            chat_path.as_ref(),
            models_path.as_ref(),
            model,
            provider_name,
        )?;
        // A configured compatible profile owns its wire contract explicitly.
        // Its user-selected name and endpoint must not opt it into Finch's
        // first-party OpenAI transport rules.
        provider.canonical_openai_endpoint = false;
        provider.canonical_meta_endpoint = false;
        provider.profile = ProviderProfile::Configured(capabilities);
        provider.compatible_tool_choice_auto = tool_choice_auto;
        provider.compatible_strict_tool_schemas = strict_tool_schemas;
        Ok(provider)
    }

    /// Create the first-party Meta Model API transport for Muse Spark.
    pub fn new_meta_model_api(api_key: String) -> Result<Self> {
        Self::new(
            api_key,
            "https://api.meta.ai".to_string(),
            "/v1/chat/completions",
            "/v1/models",
            "muse-spark-1.3".to_string(),
            "meta_model_api".to_string(),
        )
    }

    /// OpenAI-compatible transport that sends the secret as a named header
    /// instead of `Authorization: Bearer`. Used by the SuperGrok subscription
    /// lane (`xai-grok-cli`); never by Console API-key profiles.
    pub(crate) fn new_compatible_named_header(
        api_key: String,
        base_url: String,
        chat_path: impl AsRef<str>,
        models_path: impl AsRef<str>,
        default_model: String,
        provider_name: String,
        header_name: &'static str,
    ) -> Result<Self> {
        if header_name.trim().is_empty() || header_name.chars().any(char::is_control) {
            anyhow::bail!("OpenAI-compatible named auth header is invalid");
        }
        let mut provider = Self::new(
            api_key,
            base_url,
            chat_path.as_ref(),
            models_path.as_ref(),
            default_model,
            provider_name,
        )?;
        provider.auth_header = AuthHeader::Named(header_name);
        Ok(provider)
    }

    fn authorize(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.auth_header {
            AuthHeader::Bearer => {
                builder.header("Authorization", format!("Bearer {}", self.api_key))
            }
            AuthHeader::Named(name) => builder.header(name, &self.api_key),
        }
    }

    fn transport_rule(&self, model: &str) -> TransportRule {
        if self.canonical_openai_endpoint && matches!(model, "gpt-5.6-sol" | "gpt-5.6") {
            return TransportRule::CanonicalGpt56ChatCompletions;
        }
        if self.canonical_meta_endpoint {
            return TransportRule::MetaModelApiChatCompletions;
        }
        TransportRule::CompatibleChatCompletions
    }

    fn uses_strict_response_contract(&self, rule: TransportRule) -> bool {
        matches!(
            rule,
            TransportRule::CanonicalGpt56ChatCompletions
                | TransportRule::MetaModelApiChatCompletions
        ) || matches!(&self.profile, ProviderProfile::Configured(_))
    }

    fn configured_terminal_error(&self, error: anyhow::Error) -> anyhow::Error {
        if matches!(&self.profile, ProviderProfile::Configured(_)) {
            return anyhow::Error::new(NonRetriableError(format!("{error:#}")));
        }
        error
    }

    fn format_request_error(&self, error: reqwest::Error) -> anyhow::Error {
        let message = if error.is_connect() {
            format!(
                "Could not reach {} at {} — check that the server is running and the address is correct",
                self.provider_name, self.endpoints.base_url
            )
        } else if error.is_timeout() {
            format!(
                "Failed to send request to {} at {}: request timed out — check that the server is responding",
                self.provider_name, self.endpoints.base_url
            )
        } else {
            format!(
                "Failed to send request to {}: {}",
                self.provider_name, error
            )
        };
        self.configured_terminal_error(anyhow::anyhow!(message))
    }

    /// Convert a Finch request according to the explicitly selected wire rule.
    fn to_openai_request(
        &self,
        request: &ProviderRequest,
        bindings: &ToolBindingTable,
    ) -> Result<OpenAIRequest> {
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
                    TransportRule::CanonicalGpt56ChatCompletions
                    | TransportRule::MetaModelApiChatCompletions => "developer",
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
                    if matches!(
                        rule,
                        TransportRule::CanonicalGpt56ChatCompletions
                            | TransportRule::MetaModelApiChatCompletions
                    ) {
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
                                ContentBlock::Image { .. }
                                | ContentBlock::ToolResult { .. }
                                | ContentBlock::OpaqueReasoning { .. } => {
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

                    let mut tool_calls = Vec::new();
                    for block in &msg.content {
                        if let ContentBlock::ToolUse { id, name, input } = block {
                            let arguments =
                                serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
                            tool_calls.push(OpenAIRequestToolCall {
                                id: id.clone(),
                                tool_type: "function".to_string(),
                                function: OpenAIRequestFunction {
                                    name: encode_openai_tool_name(bindings, name)?,
                                    arguments,
                                },
                            });
                        }
                    }
                    if matches!(
                        rule,
                        TransportRule::CanonicalGpt56ChatCompletions
                            | TransportRule::MetaModelApiChatCompletions
                    ) {
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
                    if matches!(
                        rule,
                        TransportRule::CanonicalGpt56ChatCompletions
                            | TransportRule::MetaModelApiChatCompletions
                    ) && msg.role != "user"
                    {
                        anyhow::bail!("OpenAI request contained an unsupported message role");
                    }
                    // user/developer messages: keep ordered multimodal content for canonical
                    // OpenAI. Compatible providers retain Finch's historical text shape.
                    let mut content_parts: Vec<OpenAIContentPart> = Vec::new();
                    let mut compatible_text_parts: Vec<&str> = Vec::new();
                    let mut tool_results: Vec<(String, String)> = Vec::new();
                    let mut has_image = false;

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
                            ContentBlock::Image { source } => {
                                has_image = true;
                                match rule {
                                    TransportRule::CanonicalGpt56ChatCompletions
                                    | TransportRule::MetaModelApiChatCompletions => {
                                        content_parts.push(OpenAIContentPart::ImageUrl {
                                            image_url: validate_image_source(source)?,
                                        });
                                    }
                                    TransportRule::CompatibleChatCompletions => {
                                        compatible_text_parts.push("[image]");
                                    }
                                }
                            }
                            ContentBlock::OpaqueReasoning { .. } => {
                                anyhow::bail!("OpenAI Chat Completions cannot carry opaque Responses continuation")
                            }
                            ContentBlock::ToolUse { .. } => {
                                if matches!(
                                    rule,
                                    TransportRule::CanonicalGpt56ChatCompletions
                                        | TransportRule::MetaModelApiChatCompletions
                                ) {
                                    anyhow::bail!(
                                        "OpenAI user message contained an unsupported content block"
                                    );
                                }
                            }
                        }
                    }

                    // ToolResult+Text is Anthropic's mixed user turn (queued
                    // steering attached to the tool-result message). Split it
                    // into OpenAI tool-role messages, then a user message.
                    // Image+tool_result cannot be a legal tool-then-user split
                    // of the same payload, so keep the mix bail.
                    if has_image && !tool_results.is_empty() {
                        anyhow::bail!(
                            "OpenAI user messages cannot mix tool results with user content"
                        );
                    }

                    let content = match rule {
                        TransportRule::CanonicalGpt56ChatCompletions
                        | TransportRule::MetaModelApiChatCompletions => {
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
                    let split_steering = content.is_some() && !tool_results.is_empty();
                    if !split_steering {
                        if let Some(content) = content.clone() {
                            messages.push(OpenAIMessage::Regular {
                                role: msg.role.clone(),
                                content,
                            });
                        }
                    }

                    // One tool message per result (OpenAI requires separate messages)
                    for (tool_call_id, result) in tool_results {
                        if matches!(
                            rule,
                            TransportRule::CanonicalGpt56ChatCompletions
                                | TransportRule::MetaModelApiChatCompletions
                        ) && !outstanding_tool_ids.remove(&tool_call_id)
                        {
                            anyhow::bail!(
                                "OpenAI tool result references an unknown function call ID"
                            );
                        }
                        messages.push(OpenAIMessage::Tool {
                            role: "tool".to_string(),
                            content: if result.trim().is_empty() {
                                "(no output)".to_string()
                            } else {
                                result
                            },
                            tool_call_id,
                        });
                    }
                    if split_steering {
                        if let Some(content) = content {
                            messages.push(OpenAIMessage::Regular {
                                role: msg.role.clone(),
                                content,
                            });
                        }
                    }
                }
            }
        }
        if matches!(
            rule,
            TransportRule::CanonicalGpt56ChatCompletions
                | TransportRule::MetaModelApiChatCompletions
        ) && !outstanding_tool_ids.is_empty()
        {
            anyhow::bail!("OpenAI request contained function calls without matching results");
        }

        // Convert tools to OpenAI format if present
        let tools = if bindings.is_empty() {
            None
        } else {
            Some(
                bindings
                    .entries()
                    .iter()
                    .map(|bound| OpenAITool {
                        tool_type: "function".to_string(),
                        function: OpenAIFunction {
                            name: bound.wire.name.clone(),
                            description: bound.description.clone(),
                            parameters: bound.wire_schema.clone(),
                            strict: self.compatible_strict_tool_schemas,
                        },
                    })
                    .collect(),
            )
        };

        let cache_key = matches!(
            rule,
            TransportRule::CanonicalGpt56ChatCompletions
                | TransportRule::MetaModelApiChatCompletions
        )
        .then(|| prompt_cache_key(&model, request.system.as_deref()));
        let openai_request = OpenAIRequest {
            model,
            messages,
            max_tokens: (rule == TransportRule::CompatibleChatCompletions)
                .then_some(request.max_tokens),
            max_completion_tokens: matches!(
                rule,
                TransportRule::CanonicalGpt56ChatCompletions
                    | TransportRule::MetaModelApiChatCompletions
            )
            .then_some(request.max_tokens),
            temperature: request.temperature,
            reasoning_effort: self.reasoning_effort.map(ReasoningEffort::as_str),
            tools,
            parallel_tool_calls: (rule == TransportRule::MetaModelApiChatCompletions
                && !bindings.is_empty())
            .then_some(true),
            tool_choice: (self.compatible_tool_choice_auto && !bindings.is_empty())
                .then_some("auto"),
            stream: request.stream,
            stream_options: (request.stream
                && matches!(
                    rule,
                    TransportRule::CanonicalGpt56ChatCompletions
                        | TransportRule::MetaModelApiChatCompletions
                ))
            .then_some(OpenAIStreamOptions {
                include_usage: true,
                include_obfuscation: (rule == TransportRule::CanonicalGpt56ChatCompletions)
                    .then_some(false),
            }),
            prompt_cache_key: cache_key,
        };
        Self::validate_request_payload(&openai_request)?;
        Ok(openai_request)
    }

    /// Convert OpenAI response to ProviderResponse
    fn parse_response(
        &self,
        response: OpenAIResponse,
        rule: TransportRule,
        bindings: &ToolBindingTable,
    ) -> Result<ProviderResponse> {
        let strict_response = self.uses_strict_response_contract(rule);
        if strict_response {
            if response.object.as_deref() != Some("chat.completion") {
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
                    if strict_response
                        && (tool_call.id.is_empty()
                            || tool_call.function.name.is_empty()
                            || !call_ids.insert(tool_call.id.clone()))
                    {
                        anyhow::bail!(
                            "OpenAI returned an invalid or duplicate function call ID/name"
                        );
                    }
                    if tool_call.function.arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                        anyhow::bail!("OpenAI function arguments exceeded the 1 MiB limit");
                    }
                    let input: serde_json::Value =
                        serde_json::from_str(&tool_call.function.arguments)
                            .context("OpenAI returned malformed JSON function arguments")?;
                    if !input.is_object() {
                        anyhow::bail!("OpenAI function arguments were not a JSON object");
                    }
                    let name = decode_openai_tool_name(
                        bindings,
                        &tool_call.function.name,
                        matches!(&self.profile, ProviderProfile::Configured(_)),
                    )?;
                    content.push(ContentBlock::ToolUse {
                        id: tool_call.id,
                        name,
                        input,
                    });
                } else if strict_response {
                    anyhow::bail!("OpenAI returned an unknown tool-call type");
                }
            }
        }

        if strict_response {
            let reason = choice
                .finish_reason
                .as_deref()
                .context("OpenAI response omitted terminal status")?;
            let has_tool_calls = content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }));
            match reason {
                "stop" if !has_tool_calls => {}
                "tool_calls" if has_tool_calls => {}
                "stop" => {
                    anyhow::bail!("OpenAI response stopped despite containing function calls")
                }
                "tool_calls" => {
                    anyhow::bail!("OpenAI response reported function calls without any call items")
                }
                "length" => anyhow::bail!("OpenAI response reached its output-token limit"),
                "content_filter" => {
                    anyhow::bail!("OpenAI response was stopped by content filtering")
                }
                _ => anyhow::bail!("OpenAI returned an unknown terminal status"),
            }
        }

        Ok(ProviderResponse {
            id: response.id,
            model: response.model,
            content,
            stop_reason: choice.finish_reason,
            role: choice.message.role,
            provider: self.provider_name.clone(),
            usage: None,
            allowance: None,
        })
    }

    /// Send a single message request (no retry)
    async fn send_message_once(
        &self,
        request: &ProviderRequest,
        bindings: &ToolBindingTable,
    ) -> Result<ProviderResponse> {
        let openai_request = self.to_openai_request(request, bindings)?;
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

        let response = match self
            .authorize(self.client.post(url))
            .header("content-type", "application/json")
            .json(&openai_request)
            .send()
            .await
        {
            Ok(res) => res,
            Err(err) => return Err(self.format_request_error(err)),
        };

        let status = response.status();

        if !status.is_success() {
            let msg =
                read_api_error(response, status, self.uses_strict_response_contract(rule)).await;
            if status.is_client_error()
                && !(rule == TransportRule::MetaModelApiChatCompletions
                    && status == reqwest::StatusCode::TOO_MANY_REQUESTS)
            {
                return Err(anyhow::Error::new(NonRetriableError(msg)));
            }
            anyhow::bail!("{}", msg);
        }

        let strict_response = self.uses_strict_response_contract(rule);
        let parsed: Result<OpenAIResponse> = async {
            if strict_response {
                let body = read_bounded_response_body(response).await?;
                let value: serde_json::Value =
                    serde_json::from_slice(&body).context("Failed to parse OpenAI API response")?;
                validate_canonical_response_shape(&value, rule)?;
                if matches!(&self.profile, ProviderProfile::Configured(_)) {
                    validate_configured_response_types(&value)?;
                }
                return serde_json::from_value(value)
                    .context("OpenAI response did not match the documented schema");
            }
            response
                .json()
                .await
                .context("Failed to parse OpenAI API response")
        }
        .await;
        let openai_response = parsed.map_err(|error| self.configured_terminal_error(error))?;

        if strict_response {
            validate_canonical_actual_model(&openai_response.model)?;
        }

        tracing::debug!(
            provider = %self.provider_name,
            choices = openai_response.choices.len(),
            "received OpenAI-compatible response"
        );

        self.parse_response(openai_response, rule, bindings)
            .map_err(|error| self.configured_terminal_error(error))
    }

    /// Send a message with streaming response (no retry)
    async fn send_message_stream_once(
        &self,
        request: &ProviderRequest,
        bindings: &ToolBindingTable,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let (tx, rx) = mpsc::channel(100);

        let mut openai_request = self.to_openai_request(request, bindings)?;
        openai_request.stream = true;
        let rule = self.transport_rule(&openai_request.model);
        if matches!(
            rule,
            TransportRule::CanonicalGpt56ChatCompletions
                | TransportRule::MetaModelApiChatCompletions
        ) {
            openai_request.stream_options = Some(OpenAIStreamOptions {
                include_usage: true,
                include_obfuscation: (rule == TransportRule::CanonicalGpt56ChatCompletions)
                    .then_some(false),
            });
        }
        Self::validate_request_payload(&openai_request)?;

        let url = &self.endpoints.chat_url;

        tracing::debug!("Sending streaming request to OpenAI API");

        let response = match self
            .authorize(self.client.post(url))
            .header("content-type", "application/json")
            .json(&openai_request)
            .send()
            .await
        {
            Ok(res) => res,
            Err(err) => return Err(self.format_request_error(err)),
        };

        let status = response.status();
        if !status.is_success() {
            let msg =
                read_api_error(response, status, self.uses_strict_response_contract(rule)).await;
            if status.is_client_error()
                && !(rule == TransportRule::MetaModelApiChatCompletions
                    && status == reqwest::StatusCode::TOO_MANY_REQUESTS)
            {
                return Err(anyhow::Error::new(NonRetriableError(msg)));
            }
            anyhow::bail!("{}", msg);
        }

        if self.uses_strict_response_contract(rule) {
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let media_type = content_type
                .split(';')
                .next()
                .map(str::trim)
                .unwrap_or_default();
            if !media_type.eq_ignore_ascii_case("text/event-stream") {
                return Err(self.configured_terminal_error(anyhow::anyhow!(
                    "OpenAI streaming response was not text/event-stream"
                )));
            }
            return Ok(spawn_canonical_stream_parser(
                response,
                rule,
                self.provider_name.clone(),
                Arc::new(bindings.clone()),
                matches!(&self.profile, ProviderProfile::Configured(_)),
            ));
        }

        // Compatible providers retain the permissive historical parser.
        let provider_name = self.provider_name.clone();
        let stream_bindings = bindings.clone();
        tokio::spawn(async move {
            tracing::debug!("[STREAM] OpenAI streaming task started");
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut accumulated_text = String::new();
            // Tool call accumulator: indexed by tool_call.index.
            // Each entry: (call_id, function_name, arguments_so_far).
            // Converted to ContentBlock::ToolUse when [DONE] arrives.
            let mut tool_call_acc: Vec<(String, String, String)> = Vec::new();
            let mut tool_delta_emitted: Vec<bool> = Vec::new();
            let mut sequence = 0u64;
            let mut actual_model = String::new();
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
                                    let blocks = match finalize_tool_calls(
                                        &tool_call_acc,
                                        false,
                                        &stream_bindings,
                                        false,
                                    ) {
                                        Ok(blocks) => blocks,
                                        Err(error) => {
                                            let _ = tx.send(Err(error)).await;
                                            done = true;
                                            break;
                                        }
                                    };
                                    for block in blocks {
                                        if let ContentBlock::ToolUse {
                                            ref name,
                                            ref id,
                                            ref input,
                                        } = block
                                        {
                                            tracing::debug!(
                                                "[STREAM] Sending tool call: {} ({})",
                                                name,
                                                id
                                            );
                                            sequence += 1;
                                            if tx
                                                .send(Ok(StreamChunk::ToolCallComplete {
                                                    id: id.clone(),
                                                    name: name.clone(),
                                                    input: input.clone(),
                                                    provenance: EventProvenance {
                                                        provider: provider_name.clone(),
                                                        model: actual_model.clone(),
                                                        event: "tool_call".to_string(),
                                                        sequence,
                                                        opaque_replay: None,
                                                    },
                                                }))
                                                .await
                                                .is_err()
                                            {
                                                break;
                                            }
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
                                    if actual_model.is_empty() && !stream_chunk.model.is_empty() {
                                        actual_model = stream_chunk.model.clone();
                                    }
                                    if let Some(choice) = stream_chunk.choices.into_iter().next() {
                                        if let Some(reason) = choice.finish_reason.as_deref() {
                                            let error = match reason {
                                                "stop" | "tool_calls" | "function_call" => None,
                                                "length" => Some(anyhow::anyhow!(
                                                    "OpenAI-compatible stream reached its output-token limit"
                                                )),
                                                "content_filter" => Some(anyhow::anyhow!(
                                                    "OpenAI-compatible stream was stopped by content filtering"
                                                )),
                                                _ => Some(anyhow::anyhow!(
                                                    "OpenAI-compatible stream returned unknown finish reason '{reason}'"
                                                )),
                                            };
                                            if let Some(error) = error {
                                                let _ = tx.send(Err(error)).await;
                                                done = true;
                                                break;
                                            }
                                        }
                                        if let Some(reasoning) = choice.delta.reasoning_content {
                                            sequence += 1;
                                            if tx
                                                .send(Ok(StreamChunk::ThinkingDelta {
                                                    text: reasoning,
                                                    provenance: EventProvenance {
                                                        provider: provider_name.clone(),
                                                        model: actual_model.clone(),
                                                        event: "reasoning".to_string(),
                                                        sequence,
                                                        opaque_replay: None,
                                                    },
                                                }))
                                                .await
                                                .is_err()
                                            {
                                                done = true;
                                                break;
                                            }
                                        }
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
                                                let args_fragment = tc
                                                    .function
                                                    .as_ref()
                                                    .and_then(|function| function.arguments.clone())
                                                    .unwrap_or_default();
                                                accumulate_tool_call_delta(&mut tool_call_acc, &tc);
                                                while tool_delta_emitted.len() < tool_call_acc.len()
                                                {
                                                    tool_delta_emitted.push(false);
                                                }
                                                let idx = tc.index.unwrap_or(0);
                                                let (id, name, accumulated) =
                                                    tool_call_acc[idx].clone();
                                                if id.is_empty() {
                                                    continue;
                                                }
                                                let name = if name.is_empty() {
                                                    None
                                                } else {
                                                    match decode_openai_tool_name(
                                                        &stream_bindings,
                                                        &name,
                                                        false,
                                                    ) {
                                                        Ok(name) => Some(name),
                                                        Err(error) => {
                                                            let _ = tx.send(Err(error)).await;
                                                            done = true;
                                                            break;
                                                        }
                                                    }
                                                };
                                                let first = !tool_delta_emitted[idx];
                                                tool_delta_emitted[idx] = true;
                                                sequence += 1;
                                                if tx
                                                    .send(Ok(StreamChunk::ToolCallDelta {
                                                        id,
                                                        name,
                                                        arguments_delta: if first {
                                                            accumulated
                                                        } else {
                                                            args_fragment
                                                        },
                                                        provenance: EventProvenance {
                                                            provider: provider_name.clone(),
                                                            model: actual_model.clone(),
                                                            event: "tool_call".to_string(),
                                                            sequence,
                                                            opaque_replay: None,
                                                        },
                                                    }))
                                                    .await
                                                    .is_err()
                                                {
                                                    done = true;
                                                    break;
                                                }
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

    /// Build capabilities for a `ProviderProfile::Static` instance
    /// (openai/grok/mistral/groq) from the dated per-model table below,
    /// gated on the provider's configured endpoints still being the exact
    /// canonical URL for that provider — a custom endpoint, or a
    /// provider/model pair with no table row, stays fail-closed `Unknown`.
    fn static_table_capabilities(&self, model: &str) -> ModelCapabilities {
        let canonical_endpoints = match self.provider_name.as_str() {
            "openai" => self.canonical_openai_endpoint,
            "meta_model_api" => self.canonical_meta_endpoint,
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
                ("meta_model_api", "muse-spark-1.3") => (
                    "https://dev.meta.ai/docs/overview; https://dev.meta.ai/docs/protocols/chat-completions",
                    CapabilitySupport::Supported,
                    CapabilitySupport::Supported,
                    ReasoningCapability::allowed(
                        [
                            ReasoningEffort::Minimal,
                            ReasoningEffort::Low,
                            ReasoningEffort::Medium,
                            ReasoningEffort::High,
                            ReasoningEffort::Xhigh,
                        ],
                        "2026-10-01",
                        "https://dev.meta.ai/docs/protocols/chat-completions",
                    ),
                    1_048_576,
                    Some(131_072),
                ),
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
                // No static-table row for this exact provider/model pair:
                // stay fail-closed rather than assume support.
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
        if self.provider_name == "meta_model_api" && model == "muse-spark-1.3" {
            capabilities.parallel_tool_calls = ModelFeature::static_metadata(
                CapabilitySupport::Supported,
                "2026-10-01",
                "https://dev.meta.ai/docs/overview",
            );
            capabilities.image_input = ModelFeature::static_metadata(
                CapabilitySupport::Supported,
                "2026-10-01",
                "https://dev.meta.ai/models/muse-spark",
            );
        }
        capabilities
    }

    /// Build capabilities for an Ollama-backed instance from whatever live
    /// attestation `refresh_capabilities` has already cached for `model`.
    ///
    /// The wire protocol and streaming are always known — Finch's Ollama
    /// transport always speaks OpenAI-compatible chat completions and always
    /// streams over it, facts about the adapter, not the model (Ollama's own
    /// `/api/show` capability list has no "streaming" entry to attest from,
    /// unlike "tools") — but every model-specific optional feature stays
    /// `Unknown` until a live `/api/show` attestation for this exact model
    /// has been cached.
    fn ollama_model_capabilities(
        &self,
        endpoint: &str,
        capabilities: &Arc<std::sync::Mutex<HashMap<String, OllamaLiveCapabilities>>>,
        model: &str,
    ) -> ModelCapabilities {
        let mut capabilities_result = ModelCapabilities::unknown(self.name(), model)
            .with_wire_protocol(
                WireProtocol::OpenAiChatCompletions,
                "2026-09-19",
                "Finch Ollama adapter always uses the OpenAI-compatible chat-completions transport",
            );
        capabilities_result.streaming = ModelFeature::static_metadata(
            CapabilitySupport::Supported,
            "2026-09-19",
            "Finch Ollama adapter always streams over the OpenAI-compatible chat-completions transport, for every model",
        );
        let attested = capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(model)
            .cloned();
        let Some(attested) = attested else {
            return capabilities_result;
        };
        let source = format!(
            "live Ollama /api/show at {} (fetched {})",
            endpoint,
            attested.fetched_at.to_rfc3339()
        );
        let reports = |name: &str| attested.capabilities.iter().any(|c| c == name);
        let support_for = |present: bool| {
            if present {
                CapabilitySupport::Supported
            } else {
                CapabilitySupport::Unsupported
            }
        };
        capabilities_result.tools = ModelFeature {
            support: support_for(reports("tools")),
            provenance: CapabilityProvenance::RuntimeDiscovery {
                source: source.clone(),
            },
        };
        capabilities_result.image_input = ModelFeature {
            support: support_for(reports("vision")),
            provenance: CapabilityProvenance::RuntimeDiscovery { source },
        };
        capabilities_result
    }
}

#[async_trait]
impl ProviderBackend for OpenAIProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let (request, bindings) = request.into_request_for(self)?;
        with_retry(|| self.send_message_once(&request, &bindings)).await
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let (request, bindings) = request.into_request_for(self)?;
        with_retry(|| self.send_message_stream_once(&request, &bindings)).await
    }

    fn name(&self) -> &str {
        &self.provider_name
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        match &self.profile {
            ProviderProfile::Static => self.static_table_capabilities(model),
            ProviderProfile::Ollama {
                capability_endpoint,
                capabilities,
            } => self.ollama_model_capabilities(capability_endpoint, capabilities, model),
            ProviderProfile::RemoteDaemon => ModelCapabilities::unknown(self.name(), model),
            ProviderProfile::Configured(capabilities) => {
                if capabilities.model == model {
                    capabilities.clone()
                } else {
                    ModelCapabilities::unknown(self.name(), model)
                }
            }
        }
    }

    fn requested_reasoning_effort(&self, _request: &ProviderRequest) -> Option<ReasoningEffort> {
        self.reasoning_effort
    }

    async fn refresh_capabilities(&self, model: &str) {
        let (endpoint, capabilities) = match &self.profile {
            ProviderProfile::Static => {
                // No live attestation path for the static table.
                return;
            }
            ProviderProfile::Ollama {
                capability_endpoint,
                capabilities,
            } => (capability_endpoint.as_str(), capabilities),
            ProviderProfile::RemoteDaemon => {
                // No attestation path at all (issue #925 scope was Ollama-only).
                return;
            }
            ProviderProfile::Configured(_) => {
                // Configuration is the explicit attestation boundary.
                return;
            }
        };
        let already_attested = capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(model);
        if already_attested {
            return;
        }
        match fetch_ollama_capabilities(&self.client, endpoint, model).await {
            Ok(fetched) => {
                capabilities
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(
                        model.to_string(),
                        OllamaLiveCapabilities {
                            capabilities: fetched,
                            fetched_at: Utc::now(),
                        },
                    );
            }
            Err(error) => {
                // Fail closed: leave the model absent from the cache so
                // `capabilities()` keeps returning `Unknown` for it, and try
                // again on the next request rather than caching the failure.
                tracing::debug!(
                    provider = self.provider_name,
                    model,
                    error = %error,
                    "Ollama capability attestation unavailable; capability remains unknown"
                );
            }
        }
    }
}

#[cfg(test)]
impl OpenAIProvider {
    fn test_bindings(&self, request: &ProviderRequest) -> Result<ToolBindingTable> {
        let model = if request.model.is_empty() {
            self.default_model.as_str()
        } else {
            request.model.as_str()
        };
        openai_bindings(&self.provider_name, request, model)
    }

    fn encode_request(&self, request: &ProviderRequest) -> Result<OpenAIRequest> {
        let bindings = self.test_bindings(request)?;
        self.to_openai_request(request, &bindings)
    }

    async fn dispatch_once(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let bindings = self.test_bindings(request)?;
        self.send_message_once(request, &bindings).await
    }

    async fn dispatch_stream(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let bindings = self.test_bindings(request)?;
        self.send_message_stream_once(request, &bindings).await
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
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "is_false")]
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<OpenAIStreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIStreamOptions {
    include_usage: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_obfuscation: Option<bool>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
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
    /// Reasoning text used by Meta, xAI, and other compatible endpoints. This is
    /// activity/reasoning, never assistant output.
    reasoning_content: Option<String>,
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
    use axum::body::Bytes;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn prompt_cache_key_is_stable_for_a_stable_prefix() {
        let first = prompt_cache_key("gpt-5.6-sol", Some("system"));
        assert_eq!(first, prompt_cache_key("gpt-5.6-sol", Some("system")));
        assert_ne!(first, prompt_cache_key("gpt-5.6-sol", Some("changed")));
        assert_ne!(first, prompt_cache_key("gpt-5.6", Some("system")));
        assert!(first.len() <= 256, "OpenAI bounds cache keys to 256 bytes");
    }

    fn test_tool_bindings(names: &[&str]) -> ToolBindingTable {
        compile_from_definitions(
            WireProtocol::OpenAiChatCompletions,
            "openai",
            "gpt-4",
            &names
                .iter()
                .map(|name| crate::ToolDefinition {
                    name: (*name).to_string(),
                    description: (*name).to_string(),
                    input_schema: crate::ToolInputSchema::simple(vec![]),
                })
                .collect::<Vec<_>>(),
            &Default::default(),
        )
        .expect("test tool bindings")
    }

    fn test_stream_state() -> CanonicalStreamState {
        CanonicalStreamState {
            rule: TransportRule::CanonicalGpt56ChatCompletions,
            provider: "openai".into(),
            response_id: None,
            model: None,
            terminal_reason: None,
            usage_seen: false,
            done: false,
            accumulated_text: String::new(),
            tool_calls: Vec::new(),
            tool_delta_emitted: Vec::new(),
            sequence: 0,
            bindings: Arc::new(test_tool_bindings(&["read", "bash", "glob", "grep"])),
            redact_wire_identities: false,
        }
    }

    const VALID_PNG_BASE64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    // Public progressive, multi-scan JPEG fixture from corkami/pocs
    // (SHA 359dd741bd56611e383690bb0483a38b2bfb9584).
    const VALID_PROGRESSIVE_JPEG_BASE64: &str = "/9j/4AAQSkZJRgABAQEASABIAAD/2wBDAAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQH/wgALCAFxAZABAREA/8QAHwAAAgICAwEBAQAAAAAAAAAAAAkICgYHAwQFAgsB/9oACAEBAAAAAL/AAAAAAAAAAAAAGLVbU8LS/VvAAAAAAAAAAAAAAPPqyQUcWiLdF4AjH25IgAAAAAAAAAAABjlB62rVAaFalop5p6O99Q3VQAAAAAAAAAAAAjpV9uG0VWoK/alY49BQda/bK4/0jwA8FZ0s5OAAAAAAAAAFLt89Nm6Mz08dWaY9G9+Af6L4Breh2/6IUJWO2K9uhpDd+oNALb19zWPQAAAAOlQd2o62SdWSUzTXMbrjrVpuQAFJ22XJcIF1juCNsFZeSJ0jn85ZFZlJnjj/ALHadIcAAAI+1A2E7cSze/ApbW+9nAQJpX/oifQBHSo4mKR9mZpv1mW7Y4Z9qP5hxDzlYxOPcIAAFYdlEPMfls30KVN1YAjhRUmQ9hzQAC6kLRkxiR7e51dbzfRzbTcbf4t5kLlfoAA4qOd5H5pCXfTx6S14EAqkzweMAAABAGr1HywvKiOVYBqbyta5PHaPTZmAgAFLS5PklJu7IVPnmT1AKK2zMbstMnAAAAx6sOrlSqyHSXCImyql3raHDO57AAV3J2M3pT3WCkrdqAPCo3XrutV4hDaDnWAAAAGja2S/Ne6dtmbtyqP8g2AAAhffTbqN95Dp0s7rABE38379SbugAAAAAAC8qtGN2e8tjbKRhwBTptdV4JyuWQ5nTqQBFVKKYzEn+tc/oAAAAY8lOIDD29fZDernHCZ89nXbwDyqNl6hTTZShpfK/oAmSuZcKhZXR8r4kGzxpkxu4AACTvzqnTvSh7AG9rJgBWFYKBt1RO801PvWdSFOB5rUgNE9ahHZealCPx5opQVrWTSDlkyJK7s2PkTK40QE3/5D1owaMkE6lQn6DwFdRVUpK4v6KMUaDNzOxNBSpLY7cUAUA3NROnktx8O59f1j4IUy+hxdfk73J5LJnGo59yX75fGjHat1+kP9EIEpJP5VnOft1FcdJW/5k2gsjABO1UawXIytdZDYVFvylJUzUedbzvrufHxvqdK3cNYXf4rH7ku27RpZrn+2MyX5u9rybtxsoP3dt1AABWeUpZIjDUqtZOVSzXrYdYN/O7rXHV5OzuTeedw+wPY12RaOX2w2vqT13XKnVZt332/SrAYbm9eW4FZdyoAAoYNzcTBerPZ2ZfWxQ9FS0K0b8jHX/wBSVuPWmZG55HtbdPOXk4ck2RYD2Z1cZ0OgRn08ZMc1SGxSnCP+vbO7Zu+AFFlozjVgIJteSzWfopWk6HIfmboP3Ow+0zJiPkSIvZe9FpdfecLGNuwV3ZxzRi6rSSzBdwV1bL64ddP4SQprEnXPYDCKsjMppLgrC3COLRdQOz62bqfn+VNf59WN7I8V1Rw9UZp51t0OY0mPW1HEONmvLCkWYctclXxUgrMT3foBCKdLuvIUzWjtuh/XxscVnNAL01Y1K23+WYpALEzZI75hkUqlJQCapaW9/bsmd/QtrVuva3ELP5A7vq9wR1+wm1xtIFVrgs5qiRO25tCxYQu4pGVNfUi24i5N+ZN5wNl9jh/TdgOnaLymJIvVnJ7kztwSVidHubmWZHlf9XJFGcuG1f5W3AZNH5736EOnK/bBWLIly9kNJ+kB5hZYweu+Bn8zYuSR1VHPq4w7C9ltvcviq2xKzDpLQ8+OD69OGOnt+ye1oq2N+g5dV+P01q80yeixXRUEt80uKlOhi15WL1uAd3b2uca4v7Oi8jPuFc9sq9mJzco6J6myymWGILgj/PKROaJlsC18Uyy4uafm8tLYS1v09e6KpEVhtRSKeZWpADrfPbOp5bx7nsdY/WOVsbzlzOuttXPsHuTnp6imdj4pOPeyTIHMpV1ablVQWWU3jcEtXJxLqwVK9D3U6ikbM66fl+VxcH3yB8XRrH6XZxP1T0yCRuTU6McmiyhteRQh7sYmASPrJZs1pNdscinRGZ8k96LsOCtHWEjfaKpNy99aWC1dAdf+/YH9uPTj8V6U3etkmM7yQ/BZjTe95e8pySPJLXWdMH9ALx6dtzkInSxS2otwvoV6qwP8vAEltZV76v8ABXh5g4/nzth2F2hvg2QwLGlivBhJJXSkztiR+8b53XvNedMP9DGGyKrZAAmSnA1OU2J1d+/ZcwKkHFPgyLB8D/n8+OLtHnbesxvsdPuXcMYNl60ibLVmeLq3Yzg0hY/LxqsXhqvFyrYABqfMati/WFVxclYHFyrJDkPV8r5+j5/nB5WwL+M6HFbl0dgjL1wQ6lXOPb+mYSt7wzSerKLDFNZX6gAAhzWVSvty1bgqdaT/AJnkRU7HX6nb6Hf/AJ4uZfoQ2MNbyf1hI7Kof7rwb1N+7VRw5XV3UTW6bufm0/pYydAAClWuyEFsbCMbjiraHFcny/jh+OHucHJuOxFdS3FObrY5nmrNP5hgG9d2LTkVgimHLsrKbdrjdgAAVv0YKXtB7VqHLzXT0ffwri+PU+ji4mGNst94y0za/Jnfh9HAcJj5JPVCEccdFFGMGHMAs6gAALtqxqXw6JbrawcATweTk9DmDvyp0/dU2Iy11krY3ShjVzYTuZeMHEs2MM93JLmT/fAAAAp1rmQklEA6X3x9jmMnk9guov0zYfycedLXw8nyLF8OgIgOyFLeTAAAAAAYTVYqYV4foDyO51e3zZHM+I2FZh+j203Qcn54+d2vZUCtiaL9pEAAAAAABDejbWLjH8exozk7/vbvzjVmrMNPafKx57c4GdcOkZFJ3sUAAAAAAAAQmTnXjyCNUPsh62F6HVBjvIGzMfefY7shTjiPGjekQbAwAAAAAAAAYis2vrrBTaQYQdHp8vP/AD+7wda3DXjtH2e3EiOGibP4AAAAAAAAEL6YiiEVaO63N4fod/ndKyH0ErYRZuuNyo1J6aRrMWcgAAAAAAABTOj/AFHloeN3vvyM0lizlsSE1QeFlk1rqlhCVGeclZ+20AAAAAAAAFInzaviSeDo7XfbNVauRo1wX4++XMrRtzRhGZ4enTbz8gAAAAAAACjb6CHF/wAr8ikBIJCEItJ/XD89g9KeFiW31L3IdiQ03TOcAAAAAAABa6Be4u/1q6eqlzYX/e/1/jq9r0P7P299JPKnleziK+JdToAAAAAAAArKw90dTmW90u9wc/D6nmcfP9d5l97Ob3jMwzv432jmwIAAAf/Z";

    // Same public fixture advanced through its second progressive scan
    // (SHA e6b9af95f5bf9d2c8f52ad16ae27c05cc726af85).
    const VALID_PROGRESSIVE_MULTI_SCAN_JPEG_BASE64: &str = "/9j/4AAQSkZJRgABAQEASABIAAD/2wBDAAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQH/wgALCAFxAZABAREA/8QAHwAAAgICAwEBAQAAAAAAAAAAAAkICgYHAwQFAgsB/9oACAEBAAAAAL/AAAAAAAAAAAAAGLVbU8LS/VvAAAAAAAAAAAAAAPPqyQUcWiLdF4AjH25IgAAAAAAAAAAABjlB62rVAaFalop5p6O99Q3VQAAAAAAAAAAAAjpV9uG0VWoK/alY49BQda/bK4/0jwA8FZ0s5OAAAAAAAAAFLt89Nm6Mz08dWaY9G9+Af6L4Breh2/6IUJWO2K9uhpDd+oNALb19zWPQAAAAOlQd2o62SdWSUzTXMbrjrVpuQAFJ22XJcIF1juCNsFZeSJ0jn85ZFZlJnjj/ALHadIcAAAI+1A2E7cSze/ApbW+9nAQJpX/oifQBHSo4mKR9mZpv1mW7Y4Z9qP5hxDzlYxOPcIAAFYdlEPMfls30KVN1YAjhRUmQ9hzQAC6kLRkxiR7e51dbzfRzbTcbf4t5kLlfoAA4qOd5H5pCXfTx6S14EAqkzweMAAABAGr1HywvKiOVYBqbyta5PHaPTZmAgAFLS5PklJu7IVPnmT1AKK2zMbstMnAAAAx6sOrlSqyHSXCImyql3raHDO57AAV3J2M3pT3WCkrdqAPCo3XrutV4hDaDnWAAAAGja2S/Ne6dtmbtyqP8g2AAAhffTbqN95Dp0s7rABE38379SbugAAAAAAC8qtGN2e8tjbKRhwBTptdV4JyuWQ5nTqQBFVKKYzEn+tc/oAAAAY8lOIDD29fZDernHCZ89nXbwDyqNl6hTTZShpfK/oAmSuZcKhZXR8r4kGzxpkxu4AACTvzqnTvSh7AG9rJgBWFYKBt1RO801PvWdSFOB5rUgNE9ahHZealCPx5opQVrWTSDlkyJK7s2PkTK40QE3/5D1owaMkE6lQn6DwFdRVUpK4v6KMUaDNzOxNBSpLY7cUAUA3NROnktx8O59f1j4IUy+hxdfk73J5LJnGo59yX75fGjHat1+kP9EIEpJP5VnOft1FcdJW/5k2gsjABO1UawXIytdZDYVFvylJUzUedbzvrufHxvqdK3cNYXf4rH7ku27RpZrn+2MyX5u9rybtxsoP3dt1AABWeUpZIjDUqtZOVSzXrYdYN/O7rXHV5OzuTeedw+wPY12RaOX2w2vqT13XKnVZt332/SrAYbm9eW4FZdyoAAoYNzcTBerPZ2ZfWxQ9FS0K0b8jHX/wBSVuPWmZG55HtbdPOXk4ck2RYD2Z1cZ0OgRn08ZMc1SGxSnCP+vbO7Zu+AFFlozjVgIJteSzWfopWk6HIfmboP3Ow+0zJiPkSIvZe9FpdfecLGNuwV3ZxzRi6rSSzBdwV1bL64ddP4SQprEnXPYDCKsjMppLgrC3COLRdQOz62bqfn+VNf59WN7I8V1Rw9UZp51t0OY0mPW1HEONmvLCkWYctclXxUgrMT3foBCKdLuvIUzWjtuh/XxscVnNAL01Y1K23+WYpALEzZI75hkUqlJQCapaW9/bsmd/QtrVuva3ELP5A7vq9wR1+wm1xtIFVrgs5qiRO25tCxYQu4pGVNfUi24i5N+ZN5wNl9jh/TdgOnaLymJIvVnJ7kztwSVidHubmWZHlf9XJFGcuG1f5W3AZNH5736EOnK/bBWLIly9kNJ+kB5hZYweu+Bn8zYuSR1VHPq4w7C9ltvcviq2xKzDpLQ8+OD69OGOnt+ye1oq2N+g5dV+P01q80yeixXRUEt80uKlOhi15WL1uAd3b2uca4v7Oi8jPuFc9sq9mJzco6J6myymWGILgj/PKROaJlsC18Uyy4uafm8tLYS1v09e6KpEVhtRSKeZWpADrfPbOp5bx7nsdY/WOVsbzlzOuttXPsHuTnp6imdj4pOPeyTIHMpV1ablVQWWU3jcEtXJxLqwVK9D3U6ikbM66fl+VxcH3yB8XRrH6XZxP1T0yCRuTU6McmiyhteRQh7sYmASPrJZs1pNdscinRGZ8k96LsOCtHWEjfaKpNy99aWC1dAdf+/YH9uPTj8V6U3etkmM7yQ/BZjTe95e8pySPJLXWdMH9ALx6dtzkInSxS2otwvoV6qwP8vAEltZV76v8ABXh5g4/nzth2F2hvg2QwLGlivBhJJXSkztiR+8b53XvNedMP9DGGyKrZAAmSnA1OU2J1d+/ZcwKkHFPgyLB8D/n8+OLtHnbesxvsdPuXcMYNl60ibLVmeLq3Yzg0hY/LxqsXhqvFyrYABqfMati/WFVxclYHFyrJDkPV8r5+j5/nB5WwL+M6HFbl0dgjL1wQ6lXOPb+mYSt7wzSerKLDFNZX6gAAhzWVSvty1bgqdaT/AJnkRU7HX6nb6Hf/AJ4uZfoQ2MNbyf1hI7Kof7rwb1N+7VRw5XV3UTW6bufm0/pYydAAClWuyEFsbCMbjiraHFcny/jh+OHucHJuOxFdS3FObrY5nmrNP5hgG9d2LTkVgimHLsrKbdrjdgAAVv0YKXtB7VqHLzXT0ffwri+PU+ji4mGNst94y0za/Jnfh9HAcJj5JPVCEccdFFGMGHMAs6gAALtqxqXw6JbrawcATweTk9DmDvyp0/dU2Iy11krY3ShjVzYTuZeMHEs2MM93JLmT/fAAAAp1rmQklEA6X3x9jmMnk9guov0zYfycedLXw8nyLF8OgIgOyFLeTAAAAAAYTVYqYV4foDyO51e3zZHM+I2FZh+j203Qcn54+d2vZUCtiaL9pEAAAAAABDejbWLjH8exozk7/vbvzjVmrMNPafKx57c4GdcOkZFJ3sUAAAAAAAAQmTnXjyCNUPsh62F6HVBjvIGzMfefY7shTjiPGjekQbAwAAAAAAAAYis2vrrBTaQYQdHp8vP/AD+7wda3DXjtH2e3EiOGibP4AAAAAAAAEL6YiiEVaO63N4fod/ndKyH0ErYRZuuNyo1J6aRrMWcgAAAAAAABTOj/AFHloeN3vvyM0lizlsSE1QeFlk1rqlhCVGeclZ+20AAAAAAAAFInzaviSeDo7XfbNVauRo1wX4++XMrRtzRhGZ4enTbz8gAAAAAAACjb6CHF/wAr8ikBIJCEItJ/XD89g9KeFiW31L3IdiQ03TOcAAAAAAABa6Be4u/1q6eqlzYX/e/1/jq9r0P7P299JPKnleziK+JdToAAAAAAAArKw90dTmW90u9wc/D6nmcfP9d5l97Ob3jMwzv432jmwIAAAf/EACMQAAEDAwQDAQEAAAAAAAAAAAgFBgcDBAkAAgoQASAwQFD/2gAIAQEAAQEA/kPY1DQ4xH8xWPxiyFCOcXqYhl/jvBshKzgKteY/dUkLhf8A8cweH/osju8gok6zmjK6IZ9nRVhf9ufYegmBLpfniaPLbS/WYovR52uBVg/uMNS7JNu+G1+BSKUocQGagVW/A0OaLfirevJHDfslp1G0jrNzYwipGeOkhkSRAcwpw1fQqsHOcvBEAHpygoU9MpXEkp+pMFPNQjg1ZoqptkhRrvpxzamRtDHy5PuGcTOThg07yw+pi8f62w/+5Nzgr34zs+NUhCciZIl+7FATNnwrYrdb8JXTiwcevKo4+n1Lwj4wEq5K8gwRQWvsdkJCX8OWozNZu+uVrjK9eWW/FfHn9XYcm46RyiHF4ZSO5VSWwn9+VLhe1yUOmL6ujHDq9O2jjf8AvNhcoJJwKJkO35JBV7cm/A3oadKPFB9TWwupv5zSmMX410VgPevMNH/kV4LNckbj4evIWicZWGJvj7uud7MbqfRluOQweM4fu17HbqKepr8euUkMWKf8mpa1GsPxMn/HNCUeHGOZhthA9MgLmYzAlUP8xgNd5wMP/o1no9mLEhDDDbyIaR5bmkxUBBopOxxq7Th0RyrjpjxjxrPTLdtSJ+DQ7lvDro2JdxH+pTzZGj4ZLEbEtzXn/wB+/VW28XO0U6ZcQcxhlKsPN89TN3l81TMjhudcpUSa4fND2yqMEb5JWBxjB4P/ACHGlWubqxuPNYUTDftTGaShq44BOhzMHJmh/wB+ylNXDu6zxjj8eRXHoeHw0Qvssh0lAi5s7finrZQZ1Cqu30HQgSsU4qVVzPY7sbMDuZO8ZD3OutmMWN8DYElllc2B7S8gk4IGPg0Z8p+IPx0QlYXy+Qp944p7Cp9QFGFo4p/bQROdtXnJyz44ppQAIcEr2esM2c2R8KksuCfm4LUacjDzCo3iTKVxNahGwwMbKIFkXzS1HbH0bSxHVjFS1yqcdOWLDLI5IKt1jh7kyHYqsCmuBhOy9zjYQXYP3LM89YqI4nmciJd+7HPEcDyTHKlLbhWx6LNvRE6VJkAVs9MoAJ0em6KsLmNsi/PyGJAQmMsVZsu8R0vtabIvoEdY47Q8mu5abce7hxJUn4wazOPuG2IKsN95jOLNrL0OY3No0w8buXCi7FLCZlDV+wKLzFA0lx8S0p2wjPeNHSw21MQ0suXYrRqh3AZCprNsXxa1542uiHow7ZmEO02ZOlvXnjncgf0YbGu4+lNIcVrivCibRXlieAwUZwG1JWkZpFmM6da382FHYmhw9NZ1FOImrP0IGwc5F9Yy5f8AVPWa3VTHlH0fTUHhbMttP0r0OAYkWm6XIENG7Q8vMQZjwM4mOr7QbKj+ZhbHjOeoszH+1e16ubjDejSBsGSW4nbjcy0JQHjqkRsTkYXLS255cmWF3kRhBrkJN1szcMsImMbRNMfE2WWmMrJdxqrT7qcdLcV4kyNGjX8L5tIYtDBGDFXXTT2UOUhxh8ynE46MB58hwJUKLJcOcv4FMNq6ZEXTRVqePSkA7VZIiVVBnt1ZLCgPsN2z+IeVosfq3yukp7cNjsj9Z5ghQpByNya0sdbQeTxMkk9b+6tTfE7UxwRkPiZO8EJC6trLKsXdEb0byuRfLQYuTviJ+ueaxFq5PmaYFDwmy6QJBih9687d1rrzsiwVQScrb3SEwXhIcFXVOWEmLriUbDlcYxeWzE3rNLKyBTeUUwwHHWR9m979eetu/wA7YyjyAlaQJ+HZ6vVzB44GURQdWEhrCHl6xs55fieS4VsIRofRtkmKBX6tK9tq314rtPC0pxcuI0dumyovGzhWRJA2PCUpvAgvs8QS/FhkW0R1tJEfK4Ybjob/ABv2+bbbC4wJw3Kt2lO+x27Hc1IPKWO35IWOrrl9jT8cmhltUFpYPFTXrV11qqdu6q6hjBOTgstSqzm7VR1S1mgYCPPoeVx7qt/ia+WUjdMLshR5mh1d2N7Q7TGZJ3HDPjHamVGYoWCSstcuZW2IKU0WQmfXIETR+ete3uaHTOHibY/w5FbjZjJKb6XWs6ZIPwfB+/FJM3ZH9vpcWyhbU0+5vdR5i+lobIyjKUU/caEsCYL/AOUzDTmdNpSn58WC5HzZ3unpuQS3sX0H05VaV6PX6TkKp+J8ouyq2njLzw2ds9Txdys2kB+wg+vP63bLgwzCVc7XlXW3W3UJY951hSCYrk92trH7+w+mjNM+6qb/ABS1bDYAJXz4HOPOJGM/GdFsZfrko6SUUKVTxdN8d53Do+VXTPjTDOpNJNUav64yI0l7nVRiqzTheQn3u2aooWN+1aiQ7MkeF79UYvmaZuaY0MItXm7+qtDq8gYVhzd6JbmaIv6ckcRRDkCm/wAEreX1pUp+Laj40HYZqYRRTIe+exT/AE8gN2T+bKjb3FGjZ6sbdRT9YwosP6M7WTE6LxQ+f//Z";

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

    fn public_canonical_test_provider(base_url: String) -> OpenAIProvider {
        let mut provider = OpenAIProvider::new_openai("test-secret".to_string())
            .unwrap()
            .with_model("gpt-5.6-sol")
            .with_reasoning_effort(ReasoningEffort::High);
        provider.endpoints.chat_url = format!("{base_url}/v1/chat/completions");
        provider
    }

    fn meta_test_provider(base_url: String) -> OpenAIProvider {
        let mut provider = OpenAIProvider::new_compatible(
            "LLM|test-id|test-secret".to_string(),
            base_url,
            "/v1/chat/completions",
            "/v1/models",
            "muse-spark-1.3".to_string(),
            "meta_model_api".to_string(),
        )
        .unwrap()
        .with_reasoning_effort(ReasoningEffort::High);
        provider.canonical_meta_endpoint = true;
        provider
    }

    async fn http2_response_server(
        response_body: Option<&'static str>,
        empty_data_frames: usize,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("HTTP/2 fixture must bind a kernel-assigned loopback port");
        let address = listener
            .local_addr()
            .expect("HTTP/2 fixture must expose its bound address");
        let serving = tokio::spawn(async move {
            let (socket, _) = listener
                .accept()
                .await
                .expect("HTTP/2 fixture must accept the provider connection");
            let mut connection = h2::server::handshake(socket)
                .await
                .expect("HTTP/2 fixture handshake must complete");
            let (request, mut respond) = connection
                .accept()
                .await
                .expect("HTTP/2 fixture connection must remain open")
                .expect("HTTP/2 fixture must receive a request");
            assert_eq!(
                request.uri().path(),
                "/v1/chat/completions",
                "provider must send the request to the configured chat endpoint"
            );
            let response = axum::http::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(())
                .expect("HTTP/2 fixture response must be valid");
            let mut stream = respond
                .send_response(response, false)
                .expect("HTTP/2 fixture must send response headers");
            for frame_index in 0..empty_data_frames {
                stream
                    .send_data(Bytes::new(), false)
                    .unwrap_or_else(|error| {
                        panic!(
                            "HTTP/2 fixture failed to send empty DATA frame {frame_index}: {error}"
                        )
                    });
            }
            if let Some(body) = response_body {
                stream
                    .send_data(Bytes::from_static(body.as_bytes()), true)
                    .expect("HTTP/2 fixture must send its terminal response body");
            }
            while connection.accept().await.is_some() {}
        });
        (format!("http://{address}"), serving)
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

    async fn meta_retriable_errors_then_success_server() -> (String, mpsc::UnboundedReceiver<usize>)
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            for attempt in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 32 * 1024];
                let _ = socket.read(&mut request).await;
                let _ = accepted_tx.send(attempt);
                let (status, body) = match attempt {
                    0 => (
                        "429 Too Many Requests",
                        r#"{"error":{"message":"quota detail must be redacted","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#,
                    ),
                    1 => (
                        "503 Service Unavailable",
                        r#"{"error":{"message":"upstream detail must be redacted"}}"#,
                    ),
                    _ => (
                        "200 OK",
                        r#"{"id":"meta-ok","object":"chat.completion","model":"muse-spark-1.3","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
                    ),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.flush().await.unwrap();
            }
        });
        (format!("http://{address}"), accepted_rx)
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
            .dispatch_stream(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
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

    async fn meta_stream_outcome(body: String) -> (bool, Vec<String>) {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;
        let mut receiver = meta_test_provider(server.url())
            .send_message_stream(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("muse-spark-1.3"),
            )
            .await
            .expect("the Meta HTTP stream must start");
        let mut complete = false;
        let mut errors = Vec::new();
        while let Some(item) = receiver.recv().await {
            match item {
                Ok(StreamChunk::ContentBlockComplete(_)) => complete = true,
                Err(error) => errors.push(error.to_string()),
                _ => {}
            }
        }
        (complete, errors)
    }

    #[test]
    fn meta_model_api_constructor_pins_current_origin_model_and_capabilities() {
        let provider = OpenAIProvider::new_meta_model_api("LLM|id|secret".into())
            .expect("the fixed Meta transport must construct");
        assert_eq!(
            provider.endpoints.chat_url,
            "https://api.meta.ai/v1/chat/completions"
        );
        assert_eq!(
            provider.endpoints.models_url,
            "https://api.meta.ai/v1/models"
        );
        assert_eq!(provider.name(), "meta_model_api");
        assert_eq!(provider.default_model(), "muse-spark-1.3");
        let capabilities = provider.capabilities("muse-spark-1.3");
        assert!(capabilities.streaming.is_supported());
        assert!(capabilities.tools.is_supported());
        assert!(capabilities.parallel_tool_calls.is_supported());
        assert!(capabilities
            .reasoning
            .allowed_efforts
            .as_ref()
            .is_some_and(|efforts| efforts.contains(&ReasoningEffort::High)));
        assert_eq!(
            provider.capabilities("muse-spark-future").tools.support,
            CapabilitySupport::Unknown,
            "undocumented future Muse models must remain fail-closed"
        );
    }

    #[tokio::test]
    async fn meta_model_api_reasoning_efforts_are_exact_before_http_dispatch() {
        let mut server = mockito::Server::new_async().await;
        let no_http = server
            .mock("POST", "/v1/chat/completions")
            .expect(0)
            .with_status(500)
            .create_async()
            .await;
        let base = meta_test_provider(server.url());
        let documented = vec![
            ReasoningEffort::Minimal,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Xhigh,
        ];
        assert_eq!(
            base.capabilities("muse-spark-1.3")
                .reasoning
                .allowed_efforts,
            Some(documented.clone()),
            "Muse Spark must expose exactly Meta's documented reasoning efforts"
        );
        for effort in documented {
            let provider = base.clone().with_reasoning_effort(effort);
            crate::validate_provider_request(
                &provider,
                &ProviderRequest::new(vec![]).with_model("muse-spark-1.3"),
                false,
            )
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "documented Meta effort {} was rejected: {error}",
                    effort.as_str()
                )
            });
        }
        for effort in [ReasoningEffort::None, ReasoningEffort::Max] {
            let error = base
                .clone()
                .with_reasoning_effort(effort)
                .send_message(
                    &ProviderRequest::new(vec![crate::Message::user("hello")])
                        .with_model("muse-spark-1.3"),
                )
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&format!(
                    "does not support reasoning effort '{}'",
                    effort.as_str()
                )),
                "undocumented Meta effort must fail at validated dispatch: effort={} error={error}",
                effort.as_str()
            );
        }
        no_http.assert_async().await;
    }

    #[tokio::test]
    async fn meta_model_api_output_limit_rejects_oversized_request_before_http() {
        let mut server = mockito::Server::new_async().await;
        let accepted = server
            .mock("POST", "/v1/chat/completions")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "max_completion_tokens": 131_072
            })))
            .expect(1)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": "meta-output-limit",
                    "object": "chat.completion",
                    "model": "muse-spark-1.3",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let rejected = server
            .mock("POST", "/v1/chat/completions")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "max_completion_tokens": 131_073
            })))
            .expect(0)
            .with_status(400)
            .create_async()
            .await;
        let provider = meta_test_provider(server.url());
        assert_eq!(
            provider
                .capabilities("muse-spark-1.3")
                .output_token_limit
                .max_tokens,
            Some(131_072),
            "Muse Spark must advertise Meta's exact documented output-token maximum"
        );

        let response = provider
            .clone()
            .send_message(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("muse-spark-1.3")
                    .with_max_tokens(131_072),
            )
            .await
            .expect("Meta's documented 131072-token output maximum must be accepted");
        assert_eq!(
            response.model, "muse-spark-1.3",
            "the accepted boundary request must preserve Muse Spark model identity"
        );
        assert_eq!(
            response.provider, "meta_model_api",
            "the accepted boundary request must preserve direct Meta provider identity"
        );

        let error = provider
            .send_message(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("muse-spark-1.3")
                    .with_max_tokens(131_073),
            )
            .await
            .expect_err("an output request above Meta's maximum must fail before HTTP")
            .to_string();
        assert_eq!(
            error,
            "Provider 'meta_model_api' model 'muse-spark-1.3' supports at most 131072 output tokens, but 131073 were requested",
            "the oversized request must be rejected by capability validation before HTTP"
        );
        accepted.assert_async().await;
        rejected.assert_async().await;
    }

    #[tokio::test]
    async fn meta_model_api_stream_posts_exact_auth_body_and_preserves_parallel_tool_identity() {
        let mut server = mockito::Server::new_async().await;
        let body = concat!(
            "data: {\"id\":\"meta-1\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"checking \"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"meta-1\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"pa\"}},{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"meta-1\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a\\\"}\"}},{\"index\":1,\"function\":{\"arguments\":\"th\\\":\\\"b\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"meta-1\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"id\":\"meta-1\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":3,\"total_tokens\":11}}\n\n",
            "data: [DONE]\n\n"
        );
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer LLM|test-id|test-secret")
            .match_header("content-type", "application/json")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;
        let provider = meta_test_provider(server.url());
        let request = ProviderRequest::new(vec![crate::Message::user("use tools")])
            .with_model("muse-spark-1.3")
            .with_system("guard")
            .with_stream(true)
            .with_tools(vec![crate::ToolDefinition {
                name: "read".into(),
                description: "read".into(),
                input_schema: crate::ToolInputSchema::simple(vec![]),
            }]);
        let bindings = provider
            .test_bindings(&request)
            .expect("the Meta test tool must bind");
        let payload = serde_json::to_value(
            provider
                .to_openai_request(&request, &bindings)
                .expect("the Meta request must serialize"),
        )
        .expect("the Meta request must be JSON");
        assert_eq!(payload["model"], "muse-spark-1.3");
        assert_eq!(
            payload["messages"][0]["role"], "developer",
            "Meta request body used the wrong system-message shape: {payload}"
        );
        assert_eq!(payload["messages"][0]["content"], "guard");
        assert_eq!(payload["messages"][1]["role"], "user");
        assert_eq!(payload["max_completion_tokens"], 4096);
        assert_eq!(payload["reasoning_effort"], "high");
        assert_eq!(payload["parallel_tool_calls"], true);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["stream_options"]["include_usage"], true);
        assert!(
            payload["stream_options"]
                .get("include_obfuscation")
                .is_none(),
            "the Meta request must not send OpenAI-only stream options: {payload}"
        );
        assert_eq!(payload["tools"][0]["type"], "function");
        assert_eq!(payload["tools"][0]["function"]["name"], "read");
        let mut receiver = provider
            .send_message_stream(&request)
            .await
            .expect("the Meta stream must start");
        let mut calls = Vec::new();
        let mut model = None;
        let mut text = String::new();
        while let Some(item) = receiver.recv().await {
            match item.expect("the documented Meta stream must decode") {
                StreamChunk::ResponseMetadata { model: actual } => model = Some(actual),
                StreamChunk::TextDelta(delta) => text.push_str(&delta),
                StreamChunk::ToolCallComplete {
                    id,
                    name,
                    input,
                    provenance,
                } => {
                    calls.push((id, name, input, provenance.provider, provenance.model));
                }
                _ => {}
            }
        }
        assert_eq!(model.as_deref(), Some("muse-spark-1.3"));
        assert_eq!(text, "checking ", "Meta text deltas must preserve identity");
        assert_eq!(
            calls.len(),
            2,
            "both parallel Meta tool calls must complete: {calls:?}"
        );
        assert_eq!(calls[0].0, "call_a");
        assert_eq!(calls[0].1, "read");
        assert_eq!(calls[0].2, serde_json::json!({"path":"a"}));
        assert_eq!(calls[1].0, "call_b");
        assert!(calls
            .iter()
            .all(|call| call.3 == "meta_model_api" && call.4 == "muse-spark-1.3"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_meta_model_api_reasoning_is_private_bounded_stream_activity_at_http_boundary() {
        let mut server = mockito::Server::new_async().await;
        let body = concat!(
            "data: {\"id\":\"meta-reasoning\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":null},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"meta-reasoning\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"meta-reasoning\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"checking \"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"meta-reasoning\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"the answer\",\"content\":\"visible answer\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"meta-reasoning\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"meta-reasoning\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":5,\"total_tokens\":13}}\n\n",
            "data: [DONE]\n\n"
        );
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;
        let mut receiver = meta_test_provider(server.url())
            .send_message_stream(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("muse-spark-1.3"),
            )
            .await
            .expect("the documented Meta reasoning stream must start");

        let mut reasoning = Vec::new();
        let mut visible_deltas = String::new();
        let mut completions = Vec::new();
        let mut usage = Vec::new();
        let mut errors = Vec::new();
        while let Some(item) = receiver.recv().await {
            match item {
                Ok(StreamChunk::ThinkingDelta { text, provenance }) => {
                    reasoning.push((text, provenance));
                }
                Ok(StreamChunk::TextDelta(text)) => visible_deltas.push_str(&text),
                Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text })) => {
                    completions.push(text);
                }
                Ok(StreamChunk::Usage {
                    input_tokens,
                    output_tokens,
                }) => usage.push((input_tokens, output_tokens)),
                Err(error) => errors.push(error.to_string()),
                _ => {}
            }
        }

        assert!(
            errors.is_empty(),
            "documented Meta reasoning must not fail the HTTP stream: {errors:?}"
        );
        assert_eq!(
            reasoning.len(),
            2,
            "each fragmented Meta reasoning delta must remain distinct activity: {reasoning:?}"
        );
        assert_eq!(
            reasoning[0].0, "checking ",
            "the first fragmented reasoning delta changed: {reasoning:?}"
        );
        assert_eq!(
            reasoning[1].0, "the answer",
            "the second fragmented reasoning delta changed: {reasoning:?}"
        );
        for (index, (_, provenance)) in reasoning.iter().enumerate() {
            assert_eq!(
                provenance.provider, "meta_model_api",
                "reasoning activity carried the wrong provider provenance: {provenance:?}"
            );
            assert_eq!(
                provenance.model, "muse-spark-1.3",
                "reasoning activity carried the wrong model provenance: {provenance:?}"
            );
            assert_eq!(
                provenance.event, "reasoning",
                "reasoning activity carried the wrong event provenance: {provenance:?}"
            );
            assert_eq!(
                provenance.sequence,
                index as u64 + 1,
                "reasoning activity sequence was not monotonic: {reasoning:?}"
            );
            assert!(
                provenance.opaque_replay.is_none(),
                "Meta reasoning must not carry opaque replay material: {provenance:?}"
            );
        }
        assert_eq!(
            visible_deltas, "visible answer",
            "Meta reasoning leaked into assistant-visible deltas"
        );
        assert_eq!(
            completions,
            vec!["visible answer"],
            "Meta must publish exactly one completed assistant block without reasoning"
        );
        assert_eq!(
            usage,
            vec![(8, 5)],
            "Meta no-op reasoning fragments must not suppress or duplicate terminal usage"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_meta_model_api_nonstream_reasoning_is_accepted_but_not_assistant_content() {
        let mut server = mockito::Server::new_async().await;
        let private_reasoning = "PRIVATE_META_REASONING";
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": "meta-buffered-reasoning",
                    "object": "chat.completion",
                    "model": "muse-spark-1.3",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "reasoning_content": private_reasoning,
                            "content": "visible answer"
                        },
                        "finish_reason": "stop"
                    }]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let response = meta_test_provider(server.url())
            .send_message(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("muse-spark-1.3"),
            )
            .await
            .expect("documented buffered Meta reasoning must be accepted");

        assert_eq!(
            response.content,
            vec![ContentBlock::Text {
                text: "visible answer".into()
            }],
            "buffered Meta reasoning must not become assistant content"
        );
        assert!(
            !format!("{response:?}").contains(private_reasoning),
            "private Meta reasoning leaked into the buffered provider response"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_meta_model_api_wrong_typed_reasoning_fails_closed_at_http_boundaries() {
        let (complete, stream_errors) = meta_stream_outcome(concat!(
            "data: {\"id\":\"meta-bad-reasoning\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":17},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"meta-bad-reasoning\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"must not complete\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"meta-bad-reasoning\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"meta-bad-reasoning\",\"object\":\"chat.completion.chunk\",\"model\":\"muse-spark-1.3\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        ).to_string()).await;
        assert!(
            !complete,
            "a wrong-typed Meta reasoning delta must not publish completion"
        );
        assert!(
            stream_errors
                .iter()
                .any(|error| error.contains("documented schema")),
            "a wrong-typed Meta reasoning delta must fail with a bounded schema error: {stream_errors:?}"
        );

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .expect(3)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": "meta-bad-buffered-reasoning",
                    "object": "chat.completion",
                    "model": "muse-spark-1.3",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "reasoning_content": {"private": "must not leak"},
                            "content": "must not complete"
                        },
                        "finish_reason": "stop"
                    }]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let error = meta_test_provider(server.url())
            .send_message(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("muse-spark-1.3"),
            )
            .await
            .expect_err("wrong-typed buffered Meta reasoning must fail closed")
            .to_string();
        assert!(
            error.contains("documented schema"),
            "wrong-typed buffered Meta reasoning must fail with a bounded schema error: {error}"
        );
        assert!(
            !error.contains("must not leak"),
            "wrong-typed private Meta reasoning leaked into its schema error: {error}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn meta_model_api_stream_rejects_malformed_and_oversized_events_without_completion() {
        let (complete, errors) = meta_stream_outcome("data: {not-json}\n\n".into()).await;
        assert!(!complete, "a malformed Meta event must not publish success");
        assert!(
            errors.iter().any(|error| error.contains("malformed JSON")),
            "the malformed Meta event must produce an actionable bounded error: {errors:?}"
        );

        let oversized = format!(
            "data: {{\"padding\":\"{}\"}}\n\n",
            "x".repeat(MAX_SSE_LINE_BYTES)
        );
        let (complete, errors) = meta_stream_outcome(oversized).await;
        assert!(
            !complete,
            "an oversized Meta event must not publish success"
        );
        assert!(
            errors.iter().any(|error| error.contains("line exceeded")),
            "the oversized Meta event must name the line bound: {errors:?}"
        );
    }

    #[tokio::test]
    async fn meta_model_api_stream_cancellation_releases_transport_without_late_success() {
        let (url, closed) = stalling_http_server(true).await;
        let receiver = meta_test_provider(url)
            .send_message_stream(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("muse-spark-1.3"),
            )
            .await
            .expect("the Meta stream must start before cancellation");
        drop(receiver);
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("cancelling the Meta stream did not release the transport")
            .expect("the Meta cancellation fixture did not observe connection close");
    }

    #[tokio::test(start_paused = true)]
    async fn meta_model_api_retries_rate_limit_and_server_errors_boundedly() {
        let (url, mut accepted) = meta_retriable_errors_then_success_server().await;
        let provider = meta_test_provider(url);
        let task = tokio::spawn(async move {
            provider
                .send_message(
                    &ProviderRequest::new(vec![crate::Message::user("hello")])
                        .with_model("muse-spark-1.3"),
                )
                .await
        });
        for attempt in 0..3 {
            assert_eq!(
                recv_without_advancing_time(&mut accepted, "a Meta API attempt").await,
                attempt,
                "Meta rate-limit and server-error retries must remain ordered and bounded"
            );
            if attempt < 2 {
                for _ in 0..10 {
                    tokio::task::yield_now().await;
                }
                tokio::time::advance(Duration::from_millis(1_001 * (1 << attempt))).await;
            }
        }
        let response = task
            .await
            .expect("the Meta retry task must not panic")
            .expect("the third bounded attempt must succeed");
        assert_eq!(response.model, "muse-spark-1.3");
        assert_eq!(response.provider, "meta_model_api");
    }

    #[tokio::test]
    async fn meta_model_api_auth_error_is_actionable_and_secret_free_without_retry() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .expect(1)
            .with_status(401)
            .with_body(r#"{"error":{"message":"reflected LLM|test-id|test-secret"}}"#)
            .create_async()
            .await;
        let error = meta_test_provider(server.url())
            .send_message(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("muse-spark-1.3"),
            )
            .await
            .expect_err("Meta 401 must fail closed")
            .to_string();
        assert!(
            error.contains("Check that your API key is correct"),
            "{error}"
        );
        assert!(error.contains("response body redacted"), "{error}");
        assert!(!error.contains("LLM|test-id|test-secret"), "{error}");
        mock.assert_async().await;
    }

    /// Opt-in live acceptance only. Never run in ordinary development or CI:
    /// it contacts a metered service and requires an explicit second guard.
    #[tokio::test]
    #[ignore = "requires FINCH_LIVE_META_MODEL_API=1 and a paid MODEL_API_KEY"]
    async fn live_meta_model_api_muse_spark_acceptance_is_explicitly_opt_in() {
        assert_eq!(
            std::env::var("FINCH_LIVE_META_MODEL_API").as_deref(),
            Ok("1"),
            "set FINCH_LIVE_META_MODEL_API=1 to acknowledge a live metered Meta request"
        );
        let key = std::env::var("MODEL_API_KEY")
            .expect("MODEL_API_KEY must contain a direct Meta Model API key");
        let provider = OpenAIProvider::new_meta_model_api(key)
            .expect("the direct Meta Model API provider must construct");
        let response = provider
            .send_message(
                &ProviderRequest::new(vec![crate::Message::user(
                    "Reply with exactly the word ready.",
                )])
                .with_model("muse-spark-1.3")
                .with_max_tokens(16),
            )
            .await
            .expect("the live Meta Model API request must succeed");
        assert_eq!(
            response.model, "muse-spark-1.3",
            "Meta must report the current requested model identity"
        );
    }

    #[tokio::test]
    async fn canonical_gpt_5_6_posts_exact_current_chat_completions_json() {
        use crate::{ContentBlock, Message};
        use crate::{ToolDefinition, ToolInputSchema};

        let mut server = mockito::Server::new_async().await;
        let jpeg = valid_jpeg_base64();
        let expected = serde_json::json!({
            "model": "gpt-5.6-sol",
            "prompt_cache_key": prompt_cache_key("gpt-5.6-sol", Some("guard")),
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
        let response = provider.dispatch_once(&request).await.unwrap();
        assert_eq!(response.model, "gpt-5.6-sol-2026-08-01");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn canonical_and_compatible_rules_are_explicit_and_separate() {
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
        let configured = OpenAIProvider::new_configured_compatible(
            "key".into(),
            "https://api.openai.com".into(),
            "/v1/chat/completions",
            "/v1/models",
            "gpt-5.6-sol".into(),
            "openai".into(),
            ModelCapabilities::configured_openai_compatible(
                "openai",
                "gpt-5.6-sol",
                Some(true),
                Some(false),
                Some(false),
                Some(false),
                Some(128_000),
                Some(8_192),
            ),
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            configured.transport_rule("gpt-5.6-sol"),
            TransportRule::CompatibleChatCompletions,
            "a generic configured profile must not inherit first-party transport identity from its display name and endpoint"
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
        let validated = crate::validate_provider_request(
            &alias,
            &ProviderRequest::new(vec![crate::Message::with_content(
                "user",
                vec![ContentBlock::image("image/png", VALID_PNG_BASE64)],
            )]),
            true,
        )
        .await
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
        let request =
            ProviderRequest::new(vec![crate::Message::user("hello")]).with_model("gpt-5.6-sol");
        let response = canonical_test_provider(nonstream_server.url())
            .dispatch_once(&request)
            .await
            .unwrap();
        let mut receiver = canonical_test_provider(stream_server.url())
            .dispatch_stream(&request)
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
            .dispatch_stream(
                &ProviderRequest::new(vec![crate::Message::user("use tools")])
                    .with_model("gpt-5.6-sol")
                    .with_tools(vec![crate::ToolDefinition {
                        name: "read".into(),
                        description: "read".into(),
                        input_schema: crate::ToolInputSchema::simple(vec![]),
                    }]),
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
                StreamChunk::Usage { input_tokens, .. } => usage = Some(input_tokens),
                StreamChunk::Allowance { .. } => {}
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
    async fn canonical_stream_emits_native_tool_call_deltas_and_complete() {
        let mut server = mockito::Server::new_async().await;
        let body = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol-actual\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"p\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol-actual\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"ath\\\":\\\"a\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol-actual\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol-actual\",\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2,\"total_tokens\":6}}\n\n",
            "data: [DONE]\n\n"
        );
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream; charset=utf-8")
            .with_body(body)
            .create_async()
            .await;
        let provider = canonical_test_provider(server.url());
        let mut rx = provider
            .dispatch_stream(
                &ProviderRequest::new(vec![crate::Message::user("use tools")])
                    .with_model("gpt-5.6-sol")
                    .with_tools(vec![crate::ToolDefinition {
                        name: "read".into(),
                        description: "read".into(),
                        input_schema: crate::ToolInputSchema::simple(vec![]),
                    }]),
            )
            .await
            .unwrap();
        let mut deltas = Vec::new();
        let mut completes = Vec::new();
        while let Some(item) = rx.recv().await {
            match item.unwrap() {
                StreamChunk::ToolCallDelta {
                    id,
                    arguments_delta,
                    ..
                } => deltas.push((id, arguments_delta)),
                StreamChunk::ToolCallComplete { id, input, .. } => completes.push((id, input)),
                _ => {}
            }
        }
        assert!(
            !deltas.is_empty(),
            "canonical OpenAI must emit ToolCallDelta fragments: {deltas:?}"
        );
        assert_eq!(deltas[0].0, "call_a");
        assert_eq!(completes.len(), 1, "one ToolCallComplete: {completes:?}");
        assert_eq!(completes[0].1, serde_json::json!({"path":"a"}));
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
            .dispatch_stream(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
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
            .dispatch_stream(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
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
    async fn canonical_request_completes_over_http2_prior_knowledge() {
        let (url, serving) = http2_response_server(
            Some(
                r#"{"id":"chat-h2","object":"chat.completion","model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
            ),
            0,
        )
        .await;
        let mut provider = canonical_test_provider(url);
        provider.client = Client::builder()
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("HTTP/2 provider client must build");

        let response = provider
            .dispatch_once(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("gpt-5.6-sol"),
            )
            .await
            .expect("a normal HTTP/2 response must complete at the provider boundary");

        assert_eq!(
            response.model, "gpt-5.6-sol",
            "the provider must preserve the model returned by a normal HTTP/2 response"
        );
        drop(provider);
        tokio::time::timeout(Duration::from_secs(2), serving)
            .await
            .expect("the normal HTTP/2 fixture must quiesce after the provider is dropped")
            .expect("the normal HTTP/2 fixture task must not panic");
    }

    #[tokio::test]
    async fn canonical_request_rejects_excess_empty_http2_data_frames() {
        let (url, serving) = http2_response_server(None, 101).await;
        let mut provider = canonical_test_provider(url);
        provider.client = Client::builder()
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("HTTP/2 provider client must build");

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            provider.dispatch_once(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("gpt-5.6-sol"),
            ),
        )
        .await
        .expect("excess empty HTTP/2 DATA frames must fail within the bounded request timeout");
        let error = result.expect_err(
            "the provider must reject a response that exceeds h2's empty DATA frame budget",
        );
        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.contains("Failed to read OpenAI response body"),
            "the provider must surface the HTTP/2 body failure with request context; got: {diagnostic}"
        );
        assert!(
            diagnostic.contains("too_many_data_frames"),
            "the provider must preserve h2's excessive empty-DATA diagnosis; got: {diagnostic}"
        );
        drop(provider);
        tokio::time::timeout(Duration::from_secs(2), serving)
            .await
            .expect("the hostile HTTP/2 fixture must quiesce after rejection")
            .expect("the hostile HTTP/2 fixture task must not panic");
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
            .dispatch_once(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
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
                    &ProviderRequest::new(vec![crate::Message::user("hello")])
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
            .dispatch_stream(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
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
                .dispatch_stream(
                    &ProviderRequest::new(vec![crate::Message::user("hello")])
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
                .dispatch_once(
                    &ProviderRequest::new(vec![crate::Message::user("hello")])
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

        let meta_only_reasoning = "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-5.6-sol\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"private\"},\"finish_reason\":null}]}\n\n";
        let (complete, errors) = canonical_stream_outcome(meta_only_reasoning.to_string()).await;
        assert!(
            !complete,
            "Meta-only reasoning_content must not relax canonical OpenAI parsing"
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("unknown delta field")),
            "canonical OpenAI must continue rejecting Meta-only response fields: {errors:?}"
        );

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
        let mut state = test_stream_state();
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
        let mut state = test_stream_state();
        assert!(canonical_stream_data(
            &mut state,
            r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{"mystery":"payload"},"finish_reason":null}]}"#
        )
        .unwrap_err()
        .to_string()
        .contains("unknown delta field"));
        let mut state = test_stream_state();
        assert!(canonical_stream_data(
            &mut state,
            r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{"role":"tool"},"finish_reason":null}]}"#
        )
        .unwrap_err()
        .to_string()
        .contains("unknown delta role"));
        let mut state = test_stream_state();
        assert!(canonical_stream_data(
            &mut state,
            r#"{"id":"x","object":"chat.completion.chunk","model":"","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#
        )
        .unwrap_err()
        .to_string()
        .contains("omitted the actual model"));
        let mut state = test_stream_state();
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
        let mut state = test_stream_state();
        canonical_stream_data(&mut state, r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#).unwrap();
        assert!(canonical_stream_data(&mut state, r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#).unwrap_err().to_string().contains("duplicate terminal"));
        let mut state = test_stream_state();
        canonical_stream_data(&mut state, r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read","arguments":"{}"}}]},"finish_reason":null}]}"#).unwrap();
        assert!(canonical_stream_data(&mut state, r#"{"id":"x","object":"chat.completion.chunk","model":"gpt-5.6-sol","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_b","function":{"arguments":""}}]},"finish_reason":null}]}"#).unwrap_err().to_string().contains("changed a function-call ID"));

        let mut state = test_stream_state();
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
        let progressive_request = ProviderRequest::new(vec![crate::Message::with_content(
            "user",
            vec![ContentBlock::image(
                "image/jpeg",
                VALID_PROGRESSIVE_MULTI_SCAN_JPEG_BASE64,
            )],
        )])
        .with_model("gpt-5.6-sol");
        provider.encode_request(&progressive_request).unwrap();
        assert!(validate_jpeg(&progressive[..progressive.len() - 2]).is_err());
        let bad_base64 = ProviderRequest::new(vec![crate::Message::with_content(
            "user",
            vec![ContentBlock::image("image/png", "not base64")],
        )])
        .with_model("gpt-5.6-sol");
        assert!(provider
            .encode_request(&bad_base64)
            .unwrap_err()
            .to_string()
            .contains("invalid base64"));
        let bad_mime = ProviderRequest::new(vec![crate::Message::with_content(
            "user",
            vec![ContentBlock::image("image/webp", "AAAA")],
        )])
        .with_model("gpt-5.6-sol");
        assert!(provider
            .encode_request(&bad_mime)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
        for (media_type, data) in [("image/png", "iVBORw0KGgo="), ("image/jpeg", "/9j/")] {
            let truncated = ProviderRequest::new(vec![crate::Message::with_content(
                "user",
                vec![ContentBlock::image(media_type, data)],
            )])
            .with_model("gpt-5.6-sol");
            assert!(provider.encode_request(&truncated).is_err());
        }
        for corrupt in [corrupted_png(true), corrupted_png(false)] {
            let request = ProviderRequest::new(vec![crate::Message::with_content(
                "user",
                vec![ContentBlock::image("image/png", corrupt)],
            )])
            .with_model("gpt-5.6-sol");
            assert!(provider
                .encode_request(&request)
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
        let mismatch = ProviderRequest::new(vec![crate::Message::with_content(
            "user",
            vec![ContentBlock::tool_result(
                "missing".into(),
                "x".into(),
                None,
            )],
        )])
        .with_model("gpt-5.6-sol");
        assert!(provider
            .encode_request(&mismatch)
            .unwrap_err()
            .to_string()
            .contains("unknown function call ID"));
        let image_and_tool = ProviderRequest::new(vec![
            crate::Message::with_content(
                "assistant",
                vec![ContentBlock::ToolUse {
                    id: "call_x".into(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                }],
            ),
            crate::Message::with_content(
                "user",
                vec![
                    ContentBlock::tool_result("call_x".into(), "ok".into(), None),
                    ContentBlock::image("image/png", VALID_PNG_BASE64),
                ],
            ),
        ])
        .with_model("gpt-5.6-sol")
        .with_tools(vec![crate::ToolDefinition {
            name: "read".into(),
            description: "read".into(),
            input_schema: crate::ToolInputSchema::simple(vec![]),
        }]);
        assert_eq!(
            provider
                .encode_request(&image_and_tool)
                .unwrap_err()
                .to_string(),
            "OpenAI user messages cannot mix tool results with user content"
        );
    }

    #[test]
    fn canonical_role_blocks_and_replayed_arguments_fail_closed_at_boundaries() {
        let provider = canonical_test_provider("http://127.0.0.1:1".into());
        for block in [
            ContentBlock::image("image/png", VALID_PNG_BASE64),
            ContentBlock::tool_result("call_x".into(), "result".into(), None),
        ] {
            let request =
                ProviderRequest::new(vec![crate::Message::with_content("assistant", vec![block])])
                    .with_model("gpt-5.6-sol");
            assert!(provider
                .encode_request(&request)
                .unwrap_err()
                .to_string()
                .contains("assistant message contained an unsupported content block"));
        }

        let user_tool_call = ProviderRequest::new(vec![crate::Message::with_content(
            "user",
            vec![ContentBlock::ToolUse {
                id: "call_x".into(),
                name: "read".into(),
                input: serde_json::json!({}),
            }],
        )])
        .with_model("gpt-5.6-sol");
        assert!(provider
            .encode_request(&user_tool_call)
            .unwrap_err()
            .to_string()
            .contains("user message contained an unsupported content block"));

        let scalar_arguments = ProviderRequest::new(vec![crate::Message::with_content(
            "assistant",
            vec![ContentBlock::ToolUse {
                id: "call_x".into(),
                name: "read".into(),
                input: serde_json::json!("scalar"),
            }],
        )])
        .with_model("gpt-5.6-sol");
        assert!(provider
            .encode_request(&scalar_arguments)
            .unwrap_err()
            .to_string()
            .contains("not a JSON object"));

        let exact_string_bytes = MAX_TOOL_ARGUMENT_BYTES - r#"{"data":""}"#.len();
        let exact_input = serde_json::json!({"data": "x".repeat(exact_string_bytes)});
        assert_eq!(
            serde_json::to_string(&exact_input).unwrap().len(),
            MAX_TOOL_ARGUMENT_BYTES
        );
        let read_tool = crate::ToolDefinition {
            name: "read".into(),
            description: "read".into(),
            input_schema: crate::ToolInputSchema::simple(vec![]),
        };
        let matched = |input| {
            ProviderRequest::new(vec![
                crate::Message::with_content(
                    "assistant",
                    vec![ContentBlock::ToolUse {
                        id: "call_x".into(),
                        name: "read".into(),
                        input,
                    }],
                ),
                crate::Message::with_content(
                    "user",
                    vec![ContentBlock::tool_result(
                        "call_x".into(),
                        "ok".into(),
                        None,
                    )],
                ),
            ])
            .with_model("gpt-5.6-sol")
            .with_tools(vec![read_tool.clone()])
        };
        provider.encode_request(&matched(exact_input)).unwrap();
        let over_input = serde_json::json!({"data": "x".repeat(exact_string_bytes + 1)});
        assert!(provider
            .encode_request(&matched(over_input))
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
            .encode_request(
                &ProviderRequest::new(vec![crate::Message::with_content(
                    "assistant",
                    vec![ContentBlock::ToolUse {
                        id: "call_x".into(),
                        name: "read".into(),
                        input: serde_json::json!("scalar"),
                    }],
                )])
                .with_model("compatible-model")
                .with_tools(vec![crate::ToolDefinition {
                    name: "read".into(),
                    description: "read".into(),
                    input_schema: crate::ToolInputSchema::simple(vec![]),
                }]),
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
            let request = ProviderRequest::new(vec![crate::Message::with_content(
                "user",
                vec![ContentBlock::image("image/png", data)],
            )])
            .with_model("gpt-5.6-sol");
            let error = provider.send_message(&request).await.unwrap_err();
            assert!(error.to_string().len() < 256);
        }
        for request in [
            ProviderRequest::new(vec![crate::Message::with_content(
                "assistant",
                vec![ContentBlock::image("image/png", VALID_PNG_BASE64)],
            )])
            .with_model("gpt-5.6-sol"),
            ProviderRequest::new(vec![crate::Message::with_content(
                "user",
                vec![ContentBlock::ToolUse {
                    id: "call_x".into(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                }],
            )])
            .with_model("gpt-5.6-sol"),
            ProviderRequest::new(vec![crate::Message::with_content(
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
        let empty = ProviderRequest::new(vec![crate::Message::user("")]).with_model("gpt-5.6-sol");
        let empty_size = serde_json::to_vec(&provider.encode_request(&empty).unwrap())
            .unwrap()
            .len();
        let exact = ProviderRequest::new(vec![crate::Message::user(
            "a".repeat(MAX_REQUEST_BYTES - empty_size),
        )])
        .with_model("gpt-5.6-sol");
        assert_eq!(
            serde_json::to_vec(&provider.encode_request(&exact).unwrap())
                .unwrap()
                .len(),
            MAX_REQUEST_BYTES
        );
        assert!(provider
            .dispatch_stream(&exact)
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
            .dispatch_once(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
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
            crate::Message::with_content(
                "user",
                vec![
                    ContentBlock::text(prompt_secret),
                    ContentBlock::image("image/png", VALID_PNG_BASE64),
                ],
            ),
            crate::Message::with_content(
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
            crate::Message::with_content(
                "user",
                vec![ContentBlock::tool_result(
                    "call_secret".into(),
                    "private result".into(),
                    None,
                )],
            ),
        ])
        .with_model("gpt-5.6-sol")
        .with_tools(vec![crate::ToolDefinition {
            name: "inspect".into(),
            description: "inspect".into(),
            input_schema: crate::ToolInputSchema::simple(vec![]),
        }]);
        let error = provider.dispatch_once(&request).await.unwrap_err();
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
                .dispatch_once(
                    &ProviderRequest::new(vec![crate::Message::user("hello")])
                        .with_model("gpt-5.6-sol")
                        .with_tools(vec![crate::ToolDefinition {
                            name: "read".into(),
                            description: "read".into(),
                            input_schema: crate::ToolInputSchema::simple(vec![]),
                        }]),
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
            .dispatch_once(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
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
            .dispatch_once(
                &ProviderRequest::new(vec![crate::Message::user("hello")])
                    .with_model("gpt-5.6-sol"),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.len() < 256);
        assert!(!error.contains("MALICIOUS_PRIVATE_VALUE"));

        let huge = "REQUEST_PRIVATE_VALUE".repeat(50_000);
        let invalid_role = ProviderRequest::new(vec![crate::Message::with_content(
            huge.clone(),
            vec![ContentBlock::text("x")],
        )])
        .with_model("gpt-5.6-sol");
        let invalid_mime = ProviderRequest::new(vec![crate::Message::with_content(
            "user",
            vec![ContentBlock::image(huge.clone(), "AAAA")],
        )])
        .with_model("gpt-5.6-sol");
        let duplicate_ids = ProviderRequest::new(vec![crate::Message::with_content(
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
        let unknown_result = ProviderRequest::new(vec![crate::Message::with_content(
            "user",
            vec![ContentBlock::tool_result(huge.clone(), "x".into(), None)],
        )])
        .with_model("gpt-5.6-sol");
        for request in [invalid_role, invalid_mime, duplicate_ids, unknown_result] {
            let error = provider.encode_request(&request).unwrap_err().to_string();
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
                    },
                    finish_reason: Some(huge.clone()),
                }],
                usage: None,
            },
        ] {
            let error = provider
                .parse_response(
                    response,
                    TransportRule::CanonicalGpt56ChatCompletions,
                    &test_tool_bindings(&[]),
                )
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
    async fn compatible_nonstream_malformed_tool_arguments_fail_closed() {
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
        let error = provider
            .dispatch_once(&ProviderRequest::new(vec![crate::Message::user("hello")]))
            .await
            .expect_err("malformed function arguments must fail closed, not become {{}}");
        let message = error.to_string();
        assert!(
            message.contains("malformed JSON function arguments"),
            "compatible non-stream must not coerce malformed arguments to {{}}: {message}"
        );
        assert!(
            !message.contains("{}"),
            "error must not imply empty-object execution: {message}"
        );
    }

    #[tokio::test]
    async fn compatible_stream_malformed_arguments_emit_deltas_without_complete() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(concat!(
                "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"compatible-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_x\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"not-json\"}}]},\"finish_reason\":null}]}\n\n",
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
            .dispatch_stream(
                &ProviderRequest::new(vec![crate::Message::user("hello")]).with_tools(vec![
                    crate::ToolDefinition {
                        name: "read".into(),
                        description: "read".into(),
                        input_schema: crate::ToolInputSchema::simple(vec![]),
                    },
                ]),
            )
            .await
            .unwrap();
        let mut deltas = 0usize;
        let mut completes = 0usize;
        let mut tool_blocks = 0usize;
        while let Some(item) = rx.recv().await {
            match item.unwrap() {
                StreamChunk::ToolCallDelta { .. } => deltas += 1,
                StreamChunk::ToolCallComplete { .. } => completes += 1,
                StreamChunk::ContentBlockComplete(ContentBlock::ToolUse { .. }) => tool_blocks += 1,
                _ => {}
            }
        }
        assert!(
            deltas > 0,
            "compatible stream must emit ToolCallDelta so ToolLoop can fail closed from fragments"
        );
        assert_eq!(
            completes, 0,
            "malformed compatible-stream JSON must not emit ToolCallComplete"
        );
        assert_eq!(
            tool_blocks, 0,
            "malformed compatible-stream JSON must not emit ContentBlockComplete(ToolUse)"
        );
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
            .dispatch_stream(&ProviderRequest::new(vec![crate::Message::user("hello")]))
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
            .send_message(&ProviderRequest::new(vec![crate::Message::user("hello")]))
            .await
            .unwrap();
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn configured_compatible_streams_native_tool_calls_with_declared_wire_options() {
        let mut server = mockito::Server::new_async().await;
        let body = concat!(
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_read\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer endpoint-secret")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model": "main",
                "stream": true,
                "max_tokens": 32768,
                "tool_choice": "auto",
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "read",
                        "strict": false
                    }
                }]
            })))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;
        let capabilities = ModelCapabilities::configured_openai_compatible(
            "ciru",
            "main",
            Some(true),
            Some(true),
            Some(false),
            Some(false),
            Some(262_144),
            Some(65_536),
        );
        let provider = OpenAIProvider::new_configured_compatible(
            "endpoint-secret".into(),
            server.url(),
            "/v1/chat/completions",
            "/v1/models",
            "main".into(),
            "ciru".into(),
            capabilities,
            true,
            Some(false),
        )
        .unwrap();
        let request = ProviderRequest::new(vec![crate::Message::user("read the file")])
            .with_max_tokens(32_768)
            .with_tools(vec![crate::ToolDefinition {
                name: "read".into(),
                description: "read file".into(),
                input_schema: crate::ToolInputSchema::simple(vec![("path", "path")]),
            }]);
        let mut rx = provider.dispatch_stream(&request).await.unwrap();
        let mut completed = None;
        while let Some(item) = rx.recv().await {
            if let StreamChunk::ToolCallComplete { name, input, .. } = item.unwrap() {
                completed = Some((name, input));
            }
        }
        assert_eq!(
            completed,
            Some(("read".into(), serde_json::json!({"path": "README.md"})))
        );
        assert_eq!(provider.name(), "ciru");
        assert_eq!(
            provider.capabilities("main").tools.provenance,
            CapabilityProvenance::Configuration
        );
        assert_eq!(
            provider.capabilities("other").tools.support,
            CapabilitySupport::Unknown
        );
        mock.assert_async().await;
    }

    fn configured_test_provider(base_url: String) -> OpenAIProvider {
        OpenAIProvider::new_configured_compatible(
            "configured-sentinel-secret".into(),
            base_url,
            "/v1/chat/completions",
            "/v1/models",
            "main".into(),
            "configured-test".into(),
            ModelCapabilities::configured_openai_compatible(
                "configured-test",
                "main",
                Some(true),
                Some(true),
                Some(false),
                Some(false),
                Some(262_144),
                Some(65_536),
            ),
            true,
            Some(false),
        )
        .expect("configured-compatible test provider must construct")
    }

    #[tokio::test]
    async fn test_openai_compatible_unreachable_endpoint_produces_plain_message() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let base_url = format!("http://127.0.0.1:{port}/v1");
        let provider = OpenAIProvider::new_configured_compatible(
            "configured-secret".into(),
            base_url.clone(),
            "/chat/completions",
            "/models",
            "test-model".into(),
            "loopback".into(),
            ModelCapabilities::configured_openai_compatible(
                "loopback",
                "test-model",
                Some(true),
                Some(true),
                Some(false),
                Some(false),
                Some(32_768),
                Some(4_096),
            ),
            false,
            None,
        )
        .unwrap();

        let request = ProviderRequest::new(vec![crate::Message::user("hello")]);

        // Non-streaming
        let err = provider
            .send_message(&request)
            .await
            .expect_err("unreachable loopback endpoint must fail");
        let err_msg = err.to_string();

        assert!(
            err_msg.contains(&format!("Could not reach loopback at {base_url}")),
            "error should name the entry and base url; got: {err_msg}"
        );
        assert!(
            !err_msg.contains("OpenAI API"),
            "error must not name OpenAI API for configured loopback provider; got: {err_msg}"
        );
        assert!(
            !err_msg.contains("tcp connect error") && !err_msg.contains("os error"),
            "error must not contain raw nested library error chain; got: {err_msg}"
        );

        // Streaming
        let stream_err = provider
            .dispatch_stream(&request)
            .await
            .expect_err("unreachable loopback endpoint stream must fail");
        let stream_err_msg = stream_err.to_string();

        assert!(
            stream_err_msg.contains(&format!("Could not reach loopback at {base_url}")),
            "streaming error should name the entry and base url; got: {stream_err_msg}"
        );
        assert!(
            !stream_err_msg.contains("OpenAI API"),
            "streaming error must not name OpenAI API; got: {stream_err_msg}"
        );
        assert!(
            !stream_err_msg.contains("tcp connect error") && !stream_err_msg.contains("os error"),
            "streaming error must not contain raw nested library error chain; got: {stream_err_msg}"
        );
    }

    async fn configured_stream_outcome(
        body: impl Into<Vec<u8>>,
        content_type: &str,
    ) -> (Vec<StreamChunk>, Vec<String>) {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", content_type)
            .with_body(body.into())
            .create_async()
            .await;
        let result = configured_test_provider(server.url())
            .send_message_stream(&ProviderRequest::new(vec![crate::Message::user("hello")]))
            .await;
        let mut chunks = Vec::new();
        let mut errors = Vec::new();
        match result {
            Ok(mut receiver) => {
                while let Some(item) = receiver.recv().await {
                    match item {
                        Ok(chunk) => chunks.push(chunk),
                        Err(error) => errors.push(format!("{error:#}")),
                    }
                }
            }
            Err(error) => errors.push(format!("{error:#}")),
        }
        (chunks, errors)
    }

    fn configured_terminal_stream() -> String {
        concat!(
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string()
    }

    #[tokio::test]
    async fn configured_compatible_requires_exact_sse_media_type_and_one_done_after_terminal() {
        let (chunks, errors) = configured_stream_outcome(
            configured_terminal_stream(),
            "text/event-stream; charset=utf-8",
        )
        .await;
        assert!(
            errors.is_empty(),
            "valid configured stream failed: {errors:?}"
        );
        assert!(
            chunks.iter().any(|chunk| matches!(
                chunk,
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) if text == "ok"
            )),
            "valid configured stream omitted its completed text: {chunks:?}"
        );

        for content_type in ["application/json", "text/event-streamish", ""] {
            let (chunks, errors) =
                configured_stream_outcome(configured_terminal_stream(), content_type).await;
            assert!(
                chunks.is_empty(),
                "wrong content type leaked chunks: {chunks:?}"
            );
            assert_eq!(
                errors.len(),
                1,
                "wrong content type must produce one terminal error: type={content_type:?}, errors={errors:?}"
            );
        }

        let terminal = configured_terminal_stream();
        let cases = [
            ("missing DONE", terminal.replace("data: [DONE]\n\n", "")),
            ("duplicate DONE", format!("{terminal}data: [DONE]\n\n")),
            (
                "late data",
                format!(
                    "{terminal}data: {{\"id\":\"late\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[]}}\n\n"
                ),
            ),
            (
                "DONE before terminal",
                "data: [DONE]\n\n".to_string(),
            ),
            (
                "mid-frame EOF",
                "data: {\"id\":\"chat-1\"".to_string(),
            ),
            (
                "DONE missing its blank event terminator",
                terminal
                    .strip_suffix('\n')
                    .expect("terminal fixture ends in a blank SSE delimiter")
                    .to_string(),
            ),
            (
                "two data lines in one SSE event",
                concat!(
                    "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n",
                    "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                )
                .to_string(),
            ),
        ];
        for (case, body) in cases {
            let (chunks, errors) = configured_stream_outcome(body, "text/event-stream").await;
            assert!(
                !chunks
                    .iter()
                    .any(|chunk| matches!(chunk, StreamChunk::ContentBlockComplete(_))),
                "{case} produced a successful completion: {chunks:?}"
            );
            assert_eq!(
                errors.len(),
                1,
                "{case} must produce exactly one terminal error: {errors:?}"
            );
        }
    }

    #[tokio::test]
    async fn configured_compatible_tool_binding_diagnostics_never_reflect_the_credential() {
        let secret = "configured-sentinel-secret";
        let request = ProviderRequest::new(vec![crate::Message::user("hello")]).with_tools(vec![
            crate::ToolDefinition {
                name: "read".into(),
                description: "read".into(),
                input_schema: crate::ToolInputSchema::simple(vec![]),
            },
        ]);
        let nonstream_body = serde_json::json!({
            "id": "chat-1",
            "object": "chat.completion",
            "model": "main",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": { "name": secret, "arguments": "{}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let mut nonstream_server = mockito::Server::new_async().await;
        nonstream_server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(serde_json::to_vec(&nonstream_body).unwrap())
            .create_async()
            .await;
        let error = configured_test_provider(nonstream_server.url())
            .send_message(&request)
            .await
            .expect_err("an unadvertised reflected tool name must fail closed");
        let displayed = format!("{error:#}");
        assert!(
            !displayed.contains(secret),
            "configured nonstream tool-binding diagnostic leaked the credential: {displayed}"
        );

        let stream_body = format!(
            concat!(
                "data: {{\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{{\"name\":\"{}\",\"arguments\":\"{{}}\"}}}}]}},\"finish_reason\":null}}]}}\n\n",
                "data: {{\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n",
                "data: [DONE]\n\n"
            ),
            secret
        );
        let mut stream_server = mockito::Server::new_async().await;
        stream_server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(stream_body)
            .create_async()
            .await;
        let mut receiver = configured_test_provider(stream_server.url())
            .send_message_stream(&request)
            .await
            .expect("the response-body failure must surface through the stream");
        let mut errors = Vec::new();
        while let Some(item) = receiver.recv().await {
            if let Err(error) = item {
                errors.push(format!("{error:#}"));
            }
        }
        assert_eq!(
            errors.len(),
            1,
            "configured reflected tool name must emit exactly one terminal error: {errors:?}"
        );
        assert!(
            !errors[0].contains(secret),
            "configured streaming tool-binding diagnostic leaked the credential: {}",
            errors[0]
        );
    }

    #[tokio::test]
    async fn configured_compatible_rejects_malformed_unknown_wrong_and_sparse_stream_events() {
        let events = [
            ("malformed JSON", "not-json".to_string()),
            (
                "unknown field",
                r#"{"id":"chat-1","object":"chat.completion.chunk","model":"main","unknown":true,"choices":[{"index":0,"delta":{"content":"x"},"finish_reason":null}]}"#.to_string(),
            ),
            (
                "wrong typed field",
                r#"{"id":"chat-1","object":"chat.completion.chunk","model":"main","choices":[{"index":0,"delta":{"content":7},"finish_reason":null}]}"#.to_string(),
            ),
            (
                "empty delta",
                r#"{"id":"chat-1","object":"chat.completion.chunk","model":"main","choices":[{"index":0,"delta":{},"finish_reason":null}]}"#.to_string(),
            ),
            (
                "sparse content",
                r#"{"id":"chat-1","object":"chat.completion.chunk","model":"main","choices":[{"index":0,"delta":{"content":""},"finish_reason":null}]}"#.to_string(),
            ),
            (
                "generic reasoning leak",
                r#"{"id":"chat-1","object":"chat.completion.chunk","model":"main","choices":[{"index":0,"delta":{"reasoning_content":"private"},"finish_reason":null}]}"#.to_string(),
            ),
        ];
        for (case, event) in events {
            let body = format!("data: {event}\n\ndata: [DONE]\n\n");
            let (chunks, errors) = configured_stream_outcome(body, "text/event-stream").await;
            assert!(
                !chunks
                    .iter()
                    .any(|chunk| matches!(chunk, StreamChunk::ContentBlockComplete(_))),
                "{case} produced a successful completion: {chunks:?}"
            );
            assert_eq!(
                errors.len(),
                1,
                "{case} must produce exactly one terminal error: {errors:?}"
            );
            assert!(
                !errors[0].contains("private"),
                "{case} reflected private response content: {}",
                errors[0]
            );
        }
    }

    #[tokio::test]
    async fn configured_compatible_bounds_stream_events_aggregate_and_tool_arguments() {
        let oversized_line = format!("data: {}\n\n", "x".repeat(MAX_SSE_LINE_BYTES));
        let aggregate = format!(":{}\n", "x".repeat(MAX_SSE_LINE_BYTES - 2)).repeat(5);
        let first_args = "x".repeat(MAX_TOOL_ARGUMENT_BYTES / 2 + 1);
        let second_args = "y".repeat(MAX_TOOL_ARGUMENT_BYTES / 2 + 1);
        let oversized_args = format!(
            concat!(
                "data: {{\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{{\"arguments\":\"{}\"}}}}]}},\"finish_reason\":null}}]}}\n\n",
                "data: {{\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\"{}\"}}}}]}},\"finish_reason\":null}}]}}\n\n"
            ),
            first_args, second_args
        );
        for (case, body, expected) in [
            ("event", oversized_line, "1 MiB"),
            ("aggregate", aggregate, "4 MiB"),
            ("tool arguments", oversized_args, "function arguments"),
        ] {
            let (chunks, errors) = configured_stream_outcome(body, "text/event-stream").await;
            assert_eq!(
                errors.len(),
                1,
                "oversized {case} must produce exactly one error: chunks={chunks:?}, errors={errors:?}"
            );
            assert!(
                errors[0].contains(expected),
                "oversized {case} reported the wrong bounded diagnostic: {errors:?}"
            );
            assert!(
                !chunks
                    .iter()
                    .any(|chunk| matches!(chunk, StreamChunk::ContentBlockComplete(_))),
                "oversized {case} produced a completion: {chunks:?}"
            );
        }
    }

    #[tokio::test]
    async fn configured_compatible_bounds_nonstream_and_redacts_error_bodies() {
        let mut oversized_server = mockito::Server::new_async().await;
        oversized_server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(vec![b'x'; MAX_RESPONSE_BYTES + 1])
            .create_async()
            .await;
        let error = configured_test_provider(oversized_server.url())
            .send_message(&ProviderRequest::new(vec![crate::Message::user("hello")]))
            .await
            .expect_err("oversized configured response must fail");
        assert!(
            format!("{error:#}").contains("32 MiB"),
            "oversized configured response had the wrong error: {error:#}"
        );

        let secret = "configured-sentinel-secret";
        let mut error_server = mockito::Server::new_async().await;
        error_server
            .mock("POST", "/v1/chat/completions")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(format!(
                r#"{{"error":{{"message":"Authorization: Bearer {secret} {}"}}}}"#,
                "x".repeat(MAX_ERROR_BODY_BYTES + 1)
            ))
            .create_async()
            .await;
        let error = configured_test_provider(error_server.url())
            .send_message(&ProviderRequest::new(vec![crate::Message::user("hello")]))
            .await
            .expect_err("configured provider error must fail");
        let displayed = format!("{error:#}");
        assert!(displayed.contains("response body redacted"), "{displayed}");
        assert!(
            !displayed.contains(secret),
            "secret leaked in error: {displayed}"
        );
        assert!(
            displayed.len() < 1024,
            "bounded configured error grew unexpectedly: {} bytes",
            displayed.len()
        );
    }

    #[tokio::test]
    async fn configured_compatible_nonstream_rejects_malformed_unknown_wrong_and_terminal_fields() {
        let cases = vec![
            ("malformed JSON", b"not-json".to_vec()),
            (
                "unknown field",
                br#"{"id":"chat-1","object":"chat.completion","model":"main","unknown":true,"choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#.to_vec(),
            ),
            (
                "wrong typed field",
                br#"{"id":"chat-1","object":"chat.completion","created":"yesterday","model":"main","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#.to_vec(),
            ),
            (
                "generic reasoning leak",
                br#"{"id":"chat-1","object":"chat.completion","model":"main","choices":[{"index":0,"message":{"role":"assistant","content":"ok","reasoning_content":"private"},"finish_reason":"stop"}]}"#.to_vec(),
            ),
            (
                "missing terminal status",
                br#"{"id":"chat-1","object":"chat.completion","model":"main","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":null}]}"#.to_vec(),
            ),
            (
                "unknown terminal status",
                br#"{"id":"chat-1","object":"chat.completion","model":"main","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"mystery"}]}"#.to_vec(),
            ),
            (
                "oversized tool arguments",
                serde_json::to_vec(&serde_json::json!({
                    "id": "chat-1",
                    "object": "chat.completion",
                    "model": "main",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call-1",
                                "type": "function",
                                "function": {
                                    "name": "read",
                                    "arguments": "x".repeat(MAX_TOOL_ARGUMENT_BYTES + 1)
                                }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }]
                }))
                .unwrap(),
            ),
        ];
        for (case, body) in cases {
            let mut server = mockito::Server::new_async().await;
            server
                .mock("POST", "/v1/chat/completions")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(body)
                .create_async()
                .await;
            let error = configured_test_provider(server.url())
                .send_message(&ProviderRequest::new(vec![crate::Message::user("hello")]))
                .await
                .expect_err("invalid configured-compatible response must fail");
            let displayed = format!("{error:#}");
            assert!(
                !displayed.contains("private"),
                "{case} reflected private provider content: {displayed}"
            );
            assert!(
                displayed.len() < 2048,
                "{case} produced an unexpectedly large diagnostic: {} bytes",
                displayed.len()
            );
        }
    }

    #[tokio::test]
    async fn configured_compatible_receiver_drop_cancellation_and_timeout_release_transport() {
        let (url, closed) = stalling_http_server(true).await;
        let receiver = configured_test_provider(url)
            .send_message_stream(&ProviderRequest::new(vec![crate::Message::user("hello")]))
            .await
            .expect("configured stream must return after response headers");
        drop(receiver);
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("receiver drop did not release the configured upstream transport")
            .expect("receiver-drop server did not report transport closure");

        let (url, mut accepted, mut closed) = retrying_stalling_http_server(1).await;
        let provider = configured_test_provider(url);
        let task = tokio::spawn(async move {
            provider
                .send_message_stream(&ProviderRequest::new(vec![crate::Message::user("hello")]))
                .await
        });
        assert_eq!(
            recv_without_advancing_time(&mut accepted, "configured cancellation accept").await,
            0
        );
        task.abort();
        assert_eq!(
            recv_without_advancing_time(&mut closed, "configured cancellation close").await,
            0,
            "cancelling configured dispatch did not release its only upstream attempt"
        );

        let (url, closed) = stalling_http_server(true).await;
        let mut provider = configured_test_provider(url);
        provider.client = Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .expect("short-timeout configured client must construct");
        let mut receiver = provider
            .send_message_stream(&ProviderRequest::new(vec![crate::Message::user("hello")]))
            .await
            .expect("configured timeout stream must return after response headers");
        let first = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("configured post-header timeout hung")
            .expect("configured timeout stream ended without its terminal error")
            .expect_err("configured timeout must not be a successful chunk");
        assert!(
            format!("{first:#}")
                .to_ascii_lowercase()
                .contains("timed out"),
            "configured timeout had the wrong terminal diagnostic: {first:#}"
        );
        assert!(
            receiver.recv().await.is_none(),
            "configured timeout emitted more than one terminal outcome"
        );
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("configured timeout did not release the upstream transport")
            .expect("configured timeout server did not report transport closure");
    }

    #[tokio::test]
    async fn compatible_stream_reports_reasoning_activity_without_treating_it_as_output() {
        let mut server = mockito::Server::new_async().await;
        let body = concat!(
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"checking\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;
        let provider = OpenAIProvider::new_compatible(
            "endpoint-secret".into(),
            server.url(),
            "/v1/chat/completions",
            "/v1/models",
            "main".into(),
            "ciru".into(),
        )
        .unwrap();
        let mut rx = provider
            .dispatch_stream(&ProviderRequest::new(vec![crate::Message::user("hello")]))
            .await
            .unwrap();
        let mut reasoning = Vec::new();
        let mut text = String::new();
        let mut completed = String::new();
        while let Some(item) = rx.recv().await {
            match item.unwrap() {
                StreamChunk::ThinkingDelta { text, provenance } => {
                    reasoning.push((text, provenance.event));
                }
                StreamChunk::TextDelta(delta) => text.push_str(&delta),
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                    completed.push_str(&text);
                }
                _ => {}
            }
        }
        assert_eq!(
            reasoning,
            vec![("checking".to_string(), "reasoning".to_string())],
            "generic compatible reasoning_content must remain a distinct thinking event"
        );
        assert_eq!(text, "OK", "reasoning text leaked into assistant output");
        assert_eq!(
            completed, "OK",
            "completed assistant output must exclude reasoning_content"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn compatible_stream_surfaces_output_limit_instead_of_empty_success() {
        let mut server = mockito::Server::new_async().await;
        let body = concat!(
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"still working\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;
        let provider = OpenAIProvider::new_compatible(
            "endpoint-secret".into(),
            server.url(),
            "/v1/chat/completions",
            "/v1/models",
            "main".into(),
            "ciru".into(),
        )
        .unwrap();
        let mut rx = provider
            .dispatch_stream(&ProviderRequest::new(vec![crate::Message::user("hello")]))
            .await
            .unwrap();
        let mut error = None;
        let mut completed = false;
        while let Some(item) = rx.recv().await {
            match item {
                Err(item_error) => error = Some(item_error.to_string()),
                Ok(StreamChunk::ContentBlockComplete(_)) => completed = true,
                _ => {}
            }
        }
        assert_eq!(
            error.as_deref(),
            Some("OpenAI-compatible stream reached its output-token limit"),
            "a reasoning-only truncated stream must report its real terminal cause"
        );
        assert!(
            !completed,
            "a truncated compatible stream must not publish a completed output block"
        );
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
        use crate::Message;
        use crate::ProviderRequest;
        let req =
            ProviderRequest::new(vec![Message::user("hello")]).with_system("You are helpful.");
        let openai_req = provider.encode_request(&req).unwrap();
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
        use crate::Message;
        use crate::ProviderRequest;
        let req = ProviderRequest::new(vec![Message::user("hello")]);
        let openai_req = provider.encode_request(&req).unwrap();
        // No system message — first message is user
        assert!(
            matches!(&openai_req.messages[0], OpenAIMessage::Regular { role, .. } if role == "user")
        );
    }

    #[test]
    fn test_to_openai_request_tool_calls_included() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::ProviderRequest;
        use crate::{ContentBlock, Message};
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
        ])
        .with_tools(vec![crate::ToolDefinition {
            name: "bash".into(),
            description: "bash".into(),
            input_schema: crate::ToolInputSchema::simple(vec![]),
        }]);
        let openai_req = provider.encode_request(&req).unwrap();
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
    fn openai_history_encode_fails_closed_on_unadvertised_name() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::ProviderRequest;
        use crate::{ContentBlock, Message};
        let req = ProviderRequest::new(vec![Message::with_content(
            "assistant",
            vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "finch_spawn_agent".to_string(),
                input: serde_json::json!({}),
            }],
        )]);
        let error = provider.encode_request(&req).unwrap_err().to_string();
        assert!(
            error.contains("not in this request's binding table"),
            "unadvertised history identity must fail closed, got {error}"
        );
    }

    fn mixed_tool_result_and_steering_request() -> crate::ProviderRequest {
        use crate::ProviderRequest;
        use crate::{ContentBlock, Message};
        ProviderRequest::new(vec![
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
                vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "call_1".to_string(),
                        content: "file.txt".to_string(),
                        is_error: None,
                    },
                    ContentBlock::Text {
                        text: "steer now".to_string(),
                    },
                ],
            ),
        ])
        .with_tools(vec![crate::ToolDefinition {
            name: "bash".into(),
            description: "bash".into(),
            input_schema: crate::ToolInputSchema::simple(vec![]),
        }])
    }

    fn assert_tool_then_user_steering(openai_req: &OpenAIRequest) {
        let wire = serde_json::to_value(openai_req).unwrap();
        let messages = wire["messages"]
            .as_array()
            .expect("encoded OpenAI request must have a messages array");
        let roles: Vec<&str> = messages
            .iter()
            .map(|message| message["role"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            roles,
            ["user", "assistant", "tool", "user"],
            "mixed ToolResult+Text must encode assistant(tool_calls) → tool → user, never user-before-tool; messages={messages:?}"
        );
        assert_eq!(messages[2]["tool_call_id"], "call_1");
        assert_eq!(messages[2]["content"], "file.txt");
        let steering = match &messages[3]["content"] {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join(""),
            other => panic!("steering user content must be text or parts, got {other:?}"),
        };
        assert_eq!(
            steering, "steer now",
            "queued steering text must be the trailing user message; messages={messages:?}"
        );
    }

    #[test]
    fn mixed_tool_result_and_steering_text_encodes_tool_then_user_on_canonical_and_compatible() {
        let canonical = canonical_test_provider("http://127.0.0.1:1".into());
        let encoded = canonical
            .encode_request(&mixed_tool_result_and_steering_request())
            .expect("ToolResult+Text after a matching ToolUse must split, not bail");
        assert_tool_then_user_steering(&encoded);

        let compatible = OpenAIProvider::new_openai("key".to_string()).unwrap();
        let encoded = compatible
            .encode_request(&mixed_tool_result_and_steering_request())
            .expect("compatible Chat Completions must emit tool then user, not user-before-tool");
        assert_tool_then_user_steering(&encoded);
    }

    #[test]
    fn test_to_openai_request_tool_result_becomes_tool_role() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::ProviderRequest;
        use crate::{ContentBlock, Message};
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
        ])
        .with_tools(vec![crate::ToolDefinition {
            name: "bash".into(),
            description: "bash".into(),
            input_schema: crate::ToolInputSchema::simple(vec![]),
        }]);
        let openai_req = provider.encode_request(&req).unwrap();
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
        use crate::ProviderRequest;
        use crate::{ContentBlock, Message};
        let req = ProviderRequest::new(vec![Message::with_content(
            "user",
            vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "  ".to_string(), // whitespace-only
                is_error: None,
            }],
        )]);
        let openai_req = provider.encode_request(&req).unwrap();
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
        use crate::ProviderRequest;
        use crate::{ContentBlock, Message};
        // A user message with only whitespace text should not generate a "user" message
        let req = ProviderRequest::new(vec![Message::with_content(
            "user",
            vec![ContentBlock::Text {
                text: "   ".to_string(),
            }],
        )]);
        let openai_req = provider.encode_request(&req).unwrap();
        assert!(openai_req.messages.is_empty());
    }

    #[test]
    fn test_to_openai_request_uses_fallback_model() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::ProviderRequest;
        // Request with empty model — should fall back to provider default
        let req = ProviderRequest::new(vec![]);
        let openai_req = provider.encode_request(&req).unwrap();
        assert!(!openai_req.model.is_empty());
    }

    #[test]
    fn test_to_openai_request_includes_reasoning_effort() {
        let provider = OpenAIProvider::new_openai("key".to_string())
            .unwrap()
            .with_model("gpt-5.6-sol")
            .with_reasoning_effort(ReasoningEffort::High);
        let request = ProviderRequest::new(vec![crate::Message::user("reason carefully")]);
        let openai_request = provider.encode_request(&request).unwrap();

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
        // Streaming is a static fact about Finch's Ollama adapter (it always
        // streams over the OpenAI-compatible transport), so it's known even
        // with no live attestation — unlike tools, which is per-model and
        // stays Unknown until `/api/show` has actually been queried.
        assert_eq!(
            ollama
                .capabilities(ollama.default_model())
                .streaming
                .support,
            CapabilitySupport::Supported
        );
        assert_eq!(
            ollama.capabilities(ollama.default_model()).tools.support,
            CapabilitySupport::Unknown
        );
        // remote_daemon is untouched by issue #925: every optional feature,
        // including streaming, stays Unknown with no live attestation path.
        assert_eq!(
            remote
                .capabilities(remote.default_model())
                .streaming
                .support,
            CapabilitySupport::Unknown
        );
        assert_eq!(
            remote.capabilities(remote.default_model()).tools.support,
            CapabilitySupport::Unknown
        );
    }

    #[tokio::test]
    async fn test_ollama_capabilities_supported_when_live_attestation_reports_tools() {
        // Issue #925 regression (a): a model whose live Ollama attestation
        // includes "tools" must be declared Supported, not fail-closed
        // Unknown, so a plain query that offers tools is not refused.
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/show")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"capabilities":["completion","tools"]}"#)
            .create_async()
            .await;
        let provider = OpenAIProvider::new_ollama(server.url(), "qwen2.5:7b".to_string()).unwrap();

        provider.refresh_capabilities("qwen2.5:7b").await;
        let capabilities = provider.capabilities("qwen2.5:7b");

        mock.assert_async().await;
        assert_eq!(
            capabilities.tools.support,
            CapabilitySupport::Supported,
            "expected tools Supported from a live attestation reporting \"tools\", got {:?}",
            capabilities.tools
        );
        assert!(
            matches!(
                capabilities.tools.provenance,
                CapabilityProvenance::RuntimeDiscovery { .. }
            ),
            "expected a live RuntimeDiscovery provenance distinct from StaticMetadata, got {:?}",
            capabilities.tools.provenance
        );
    }

    #[tokio::test]
    async fn test_ollama_capabilities_unsupported_when_live_attestation_omits_tools() {
        // Issue #925 regression (b): a model whose live Ollama attestation
        // does not list "tools" must be declared Unsupported (Ollama's own
        // capability list is authoritative), not left Unknown.
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/show")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"capabilities":["completion"]}"#)
            .create_async()
            .await;
        let provider = OpenAIProvider::new_ollama(server.url(), "llama3.2:1b".to_string()).unwrap();

        provider.refresh_capabilities("llama3.2:1b").await;
        let capabilities = provider.capabilities("llama3.2:1b");

        assert_eq!(
            capabilities.tools.support,
            CapabilitySupport::Unsupported,
            "expected tools Unsupported when a live attestation omits \"tools\", got {:?}",
            capabilities.tools
        );
    }

    #[tokio::test]
    async fn test_ollama_capabilities_stay_unknown_when_api_show_connection_refused() {
        // Issue #925 regression (c), transport failure: a capability check
        // that cannot reach Ollama must fail closed, never assume Supported.
        let provider =
            OpenAIProvider::new_ollama("http://127.0.0.1:1".to_string(), "qwen2.5:7b".to_string())
                .unwrap();

        provider.refresh_capabilities("qwen2.5:7b").await;
        let capabilities = provider.capabilities("qwen2.5:7b");

        assert_eq!(
            capabilities.tools.support,
            CapabilitySupport::Unknown,
            "a connection failure reaching /api/show must leave tools Unknown, not assume support; got {:?}",
            capabilities.tools
        );
    }

    #[tokio::test]
    async fn test_ollama_capabilities_stay_unknown_when_api_show_returns_malformed_json() {
        // Issue #925 regression (c), decode failure: a malformed /api/show
        // body must also fail closed rather than assume support.
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/show")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not valid json")
            .create_async()
            .await;
        let provider = OpenAIProvider::new_ollama(server.url(), "qwen2.5:7b".to_string()).unwrap();

        provider.refresh_capabilities("qwen2.5:7b").await;
        let capabilities = provider.capabilities("qwen2.5:7b");

        assert_eq!(
            capabilities.tools.support,
            CapabilitySupport::Unknown,
            "a malformed /api/show body must leave tools Unknown, not assume support; got {:?}",
            capabilities.tools
        );
    }

    #[tokio::test]
    async fn test_ollama_capabilities_stay_unknown_when_api_show_returns_server_error() {
        // Issue #925 regression (c), non-2xx status: an error status from
        // /api/show must also fail closed rather than assume support.
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/show")
            .with_status(500)
            .with_body("internal error")
            .create_async()
            .await;
        let provider = OpenAIProvider::new_ollama(server.url(), "qwen2.5:7b".to_string()).unwrap();

        provider.refresh_capabilities("qwen2.5:7b").await;
        let capabilities = provider.capabilities("qwen2.5:7b");

        assert_eq!(
            capabilities.tools.support,
            CapabilitySupport::Unknown,
            "a 500 status from /api/show must leave tools Unknown, not assume support; got {:?}",
            capabilities.tools
        );
    }

    #[tokio::test]
    async fn test_ollama_wire_protocol_known_even_without_live_attestation() {
        // The wire protocol is a fact about Finch's Ollama adapter (it always
        // speaks OpenAI-compatible chat completions), not about the model, so
        // it must be known even before any live attestation succeeds —
        // otherwise tool-binding compilation would fail closed for a
        // reason unrelated to the model's real capabilities.
        let provider =
            OpenAIProvider::new_ollama("http://127.0.0.1:1".to_string(), "qwen2.5:7b".to_string())
                .unwrap();

        assert_eq!(
            provider.capabilities("qwen2.5:7b").wire_protocol.protocol,
            Some(WireProtocol::OpenAiChatCompletions),
            "Ollama's wire protocol should be known statically regardless of live capability attestation"
        );
    }

    #[tokio::test]
    async fn test_ollama_query_with_tools_succeeds_after_live_capability_attestation() {
        // Production-boundary reproduction of the reported bug: a plain
        // request that offers tools (Finch offers tools on essentially every
        // request) used to be refused for every Ollama model with
        // "has unknown tool calls capability; refusing to assume support".
        // After a live attestation reports "tools", the same request must
        // pass validation at the real dispatch boundary.
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/show")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"capabilities":["completion","tools"]}"#)
            .create_async()
            .await;
        let provider = OpenAIProvider::new_ollama(server.url(), "qwen2.5:7b".to_string()).unwrap();
        let request = ProviderRequest::new(vec![crate::Message::user("hello")]).with_tools(vec![
            crate::ToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                input_schema: crate::ToolInputSchema::simple(vec![("path", "string")]),
            },
        ]);

        let validated = crate::validate_provider_request(&provider, &request, false)
            .await
            .expect(
                "a tool-offering request must validate once Ollama's live attestation reports \"tools\"",
            );
        assert_eq!(
            validated.capabilities().tools.support,
            CapabilitySupport::Supported
        );
    }

    #[tokio::test]
    async fn test_remote_daemon_capabilities_unaffected_by_ollama_live_attestation() {
        // Issue #925 is Ollama-only: remote_daemon must keep returning a bare
        // fail-closed descriptor, with no live capability endpoint and no
        // wire protocol implied, exactly as before this change.
        let remote = OpenAIProvider::new_remote_daemon("http://127.0.0.1:1".to_string()).unwrap();

        remote.refresh_capabilities(remote.default_model()).await;
        let capabilities = remote.capabilities(remote.default_model());

        assert_eq!(capabilities.tools.support, CapabilitySupport::Unknown);
        assert_eq!(capabilities.streaming.support, CapabilitySupport::Unknown);
        assert_eq!(capabilities.wire_protocol.protocol, None);
    }

    #[tokio::test]
    async fn test_ollama_streaming_is_supported_regardless_of_live_attestation() {
        // Code-review follow-up on #925: streaming is a fact about Finch's
        // Ollama adapter (it always streams over the OpenAI-compatible
        // transport), not a per-model feature Ollama reports in /api/show,
        // so it must be Supported even with no live attestation at all —
        // otherwise every streaming Ollama request reproduces the exact bug
        // this issue was filed for, just for "streaming" instead of "tools".
        let provider =
            OpenAIProvider::new_ollama("http://127.0.0.1:1".to_string(), "qwen2.5:7b".to_string())
                .unwrap();

        assert_eq!(
            provider.capabilities("qwen2.5:7b").streaming.support,
            CapabilitySupport::Supported,
            "Ollama streaming must be known Supported without any live attestation"
        );
    }

    #[tokio::test]
    async fn test_ollama_streaming_query_succeeds_at_the_dispatch_boundary() {
        // Production-boundary reproduction of the streaming variant of the
        // reported bug: a streaming request used to be refused with
        // "has unknown streaming capability; refusing to assume support"
        // for every Ollama model, with no live attestation able to fix it
        // (Ollama's /api/show has no "streaming" entry to report).
        let provider =
            OpenAIProvider::new_ollama("http://127.0.0.1:1".to_string(), "qwen2.5:7b".to_string())
                .unwrap();
        let request = ProviderRequest::new(vec![crate::Message::user("hello")]).with_stream(true);

        let validated = crate::validate_provider_request(&provider, &request, true)
            .await
            .expect(
                "a streaming request must validate against Ollama without any live attestation",
            );
        assert_eq!(
            validated.capabilities().streaming.support,
            CapabilitySupport::Supported
        );
    }

    // Issue #929 regression: `capabilities()` and `refresh_capabilities()`
    // both `match &self.profile { ProviderProfile::Static => ..., Ollama {
    // .. } => ..., RemoteDaemon => ... }` with no `_` wildcard arm. That
    // absence is what makes the dispatch exhaustive: `cargo build` for this
    // crate already refuses to compile if a fourth `ProviderProfile` variant
    // is added without a matching arm in both methods, because a `match`
    // without a wildcard over a non-exhaustively-covered enum is itself a
    // compiler error (E0004). A runtime test cannot independently re-prove
    // "this fails to compile without a fix" without either duplicating the
    // enum in test-only code (which would prove nothing about the real
    // `capabilities()`/`refresh_capabilities()` dispatch) or literally
    // deleting an arm, which would fail this file's own build rather than
    // one isolated test. So the practical regression coverage kept here is
    // per-variant: lock in the exact `capabilities()` shape each existing
    // `ProviderProfile` variant produces today, so a future edit that
    // changes what one variant's arm returns — including a change that
    // accidentally merges two arms' behavior — is caught by a failing
    // assertion instead of by re-reading the match.
    #[tokio::test]
    async fn test_capabilities_dispatch_covers_every_provider_profile_variant() {
        // ProviderProfile::Static (openai/grok/mistral/groq): a known
        // provider/model pair resolves from the static table with a known
        // wire protocol and Supported tools.
        let openai = OpenAIProvider::new_openai("key".to_string()).unwrap();
        let static_capabilities = openai.capabilities("gpt-4o");
        assert_eq!(
            static_capabilities.tools.support,
            CapabilitySupport::Supported,
            "Static profile: known provider/model pair must resolve from the static table, got {:?}",
            static_capabilities.tools
        );
        assert_eq!(
            static_capabilities.wire_protocol.protocol,
            Some(WireProtocol::OpenAiChatCompletions),
            "Static profile: wire protocol is a static adapter fact, must be known"
        );
        assert!(
            matches!(
                static_capabilities.tools.provenance,
                CapabilityProvenance::StaticMetadata { .. }
            ),
            "Static profile: provenance must be StaticMetadata, not live attestation, got {:?}",
            static_capabilities.tools.provenance
        );

        // ProviderProfile::Ollama, before any live attestation: fails closed
        // to Unknown for the model-specific feature, but the wire protocol
        // is still known because it is a fact about the adapter, not a
        // per-model attestation.
        let ollama =
            OpenAIProvider::new_ollama("http://127.0.0.1:1".to_string(), "qwen2.5:7b".to_string())
                .unwrap();
        let ollama_capabilities = ollama.capabilities("qwen2.5:7b");
        assert_eq!(
            ollama_capabilities.tools.support,
            CapabilitySupport::Unknown,
            "Ollama profile: unattested model must stay Unknown, not assume support, got {:?}",
            ollama_capabilities.tools
        );
        assert_eq!(
            ollama_capabilities.wire_protocol.protocol,
            Some(WireProtocol::OpenAiChatCompletions),
            "Ollama profile: wire protocol is known even without live attestation"
        );

        // ProviderProfile::RemoteDaemon: always Unknown, no wire protocol
        // implied — there is no attestation path at all for this variant.
        let remote_daemon =
            OpenAIProvider::new_remote_daemon("http://127.0.0.1:1".to_string()).unwrap();
        let remote_daemon_capabilities = remote_daemon.capabilities(remote_daemon.default_model());
        assert_eq!(
            remote_daemon_capabilities.tools.support,
            CapabilitySupport::Unknown,
            "RemoteDaemon profile: no attestation path, must stay Unknown, got {:?}",
            remote_daemon_capabilities.tools
        );
        assert_eq!(
            remote_daemon_capabilities.wire_protocol.protocol, None,
            "RemoteDaemon profile: no wire protocol should be implied"
        );

        // The three variants must be distinguishable from each other by
        // their capability shape alone — that is the property the
        // exhaustive match exists to preserve.
        assert_ne!(
            static_capabilities.tools.support, ollama_capabilities.tools.support,
            "Static and Ollama profiles must not collapse to the same capability shape for these fixtures"
        );
        assert_eq!(
            ollama_capabilities.tools.support, remote_daemon_capabilities.tools.support,
            "Ollama-before-attestation and RemoteDaemon happen to agree on Unknown tools support today, \
             but only the wire_protocol assertions above prove they took different match arms to get there"
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

    #[tokio::test]
    async fn gpt_5_6_sol_reasoning_efforts_are_exact() {
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
            crate::validate_provider_request(&provider, &ProviderRequest::new(vec![]), false)
                .await
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
        let blocks = finalize_tool_calls(
            &acc,
            true,
            &test_tool_bindings(&["bash", "glob", "grep", "read"]),
            false,
        )
        .unwrap();
        assert_eq!(blocks.len(), 1);
        if let crate::ContentBlock::ToolUse { id, name, input } = &blocks[0] {
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
        let error = finalize_tool_calls(
            &acc,
            true,
            &test_tool_bindings(&["bash", "glob", "grep", "read"]),
            false,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("malformed JSON function arguments"));
    }

    #[test]
    fn test_finalize_tool_calls_empty_acc() {
        let acc: Vec<(String, String, String)> = Vec::new();
        let blocks = finalize_tool_calls(
            &acc,
            true,
            &test_tool_bindings(&["bash", "glob", "grep", "read"]),
            false,
        )
        .unwrap();
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
        let blocks = finalize_tool_calls(
            &acc,
            true,
            &test_tool_bindings(&["bash", "glob", "grep", "read"]),
            false,
        )
        .unwrap();
        assert_eq!(blocks.len(), 1);
        if let crate::ContentBlock::ToolUse { id, name, input } = &blocks[0] {
            assert_eq!(id, "call_xyz");
            assert_eq!(name, "bash");
            assert_eq!(input["command"].as_str().unwrap(), "echo test");
        } else {
            panic!("Expected ToolUse block, got {:?}", blocks[0]);
        }
    }

    #[test]
    fn test_ollama_stream_chunk_with_unknown_fields() {
        let mut state = test_stream_state();
        state.rule = TransportRule::CompatibleChatCompletions;
        state.terminal_reason = Some("stop".to_string());

        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1677652288,"model":"qwen2.5:7b","system_fingerprint":"fp_44709d6fcb","choices":[],"usage":{"prompt_tokens":9,"completion_tokens":12,"total_tokens":21,"eval_count":12,"eval_duration":1000000000,"load_duration":1000000000,"prompt_eval_count":9,"prompt_eval_duration":1000000000,"total_duration":3000000000,"some_new_metric":42}}"#;

        let result = super::canonical_stream_data(&mut state, data);
        if let Err(e) = &result {
            println!("Error: {e}");
        }
        assert!(
            result.is_ok(),
            "Stream parser should not crash on unknown keys (e.g. from Ollama)"
        );
    }
}
