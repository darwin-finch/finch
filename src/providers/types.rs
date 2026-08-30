// Unified request/response types for multi-provider LLM support
//
// These types abstract over provider-specific formats (Claude, OpenAI, Gemini, etc.)
// allowing the rest of the codebase to work with a unified interface.

use crate::claude::types::{ContentBlock, Message};
use crate::config::ReasoningEffort;
use crate::tools::types::ToolDefinition;
use serde::{Deserialize, Serialize};

/// Whether a provider/model capability is known to be usable.
///
/// `Unknown` is deliberately distinct from `Unsupported`: callers may display
/// the uncertainty, but must not send a request that depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Supported,
    Unsupported,
    Unknown,
}

/// Where Finch obtained a capability claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapabilityProvenance {
    /// Returned by an authoritative runtime catalog or handshake.
    RuntimeDiscovery { source: String },
    /// Dated metadata verified against a named provider contract.
    StaticMetadata { tested_on: String, source: String },
    /// Asserted by a validated local configuration boundary.
    Configuration,
    /// No reliable evidence is available.
    Unknown,
}

/// A single yes/no/unknown model feature with its evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFeature {
    /// Whether the feature is known to be usable.
    pub support: CapabilitySupport,
    /// Evidence supporting the status.
    pub provenance: CapabilityProvenance,
}

impl ModelFeature {
    /// Construct an unknown feature with no provenance.
    pub fn unknown() -> Self {
        Self {
            support: CapabilitySupport::Unknown,
            provenance: CapabilityProvenance::Unknown,
        }
    }

    /// Construct a dated static feature claim.
    pub fn static_metadata(
        support: CapabilitySupport,
        tested_on: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            support,
            provenance: CapabilityProvenance::StaticMetadata {
                tested_on: tested_on.into(),
                source: source.into(),
            },
        }
    }

    /// Return true only for explicit support, never for unknown status.
    pub fn is_supported(&self) -> bool {
        self.support == CapabilitySupport::Supported
    }
}

/// Exact reasoning-effort values accepted by one provider/model adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningCapability {
    /// `None` means unknown; an empty list means reasoning controls are
    /// explicitly unsupported.
    pub allowed_efforts: Option<Vec<ReasoningEffort>>,
    pub provenance: CapabilityProvenance,
}

impl ReasoningCapability {
    pub fn unknown() -> Self {
        Self {
            allowed_efforts: None,
            provenance: CapabilityProvenance::Unknown,
        }
    }

    pub fn unsupported(tested_on: &str, source: &str) -> Self {
        Self {
            allowed_efforts: Some(Vec::new()),
            provenance: CapabilityProvenance::StaticMetadata {
                tested_on: tested_on.to_string(),
                source: source.to_string(),
            },
        }
    }

    pub fn allowed(
        efforts: impl IntoIterator<Item = ReasoningEffort>,
        tested_on: &str,
        source: &str,
    ) -> Self {
        Self {
            allowed_efforts: Some(efforts.into_iter().collect()),
            provenance: CapabilityProvenance::StaticMetadata {
                tested_on: tested_on.to_string(),
                source: source.to_string(),
            },
        }
    }

    pub fn support(&self) -> CapabilitySupport {
        match self.allowed_efforts.as_deref() {
            None => CapabilitySupport::Unknown,
            Some([]) => CapabilitySupport::Unsupported,
            Some(_) => CapabilitySupport::Supported,
        }
    }

    fn require(&self, provider: &str, model: &str, effort: ReasoningEffort) -> anyhow::Result<()> {
        match self.allowed_efforts.as_deref() {
            None => anyhow::bail!(
                "Provider '{}' model '{}' has unknown reasoning capability; refusing configured effort '{}'",
                provider,
                model,
                effort.as_str()
            ),
            Some(allowed) if allowed.is_empty() => anyhow::bail!(
                "Provider '{}' model '{}' does not support reasoning controls",
                provider,
                model
            ),
            Some(allowed) if !allowed.contains(&effort) => anyhow::bail!(
                "Provider '{}' model '{}' does not support reasoning effort '{}'; allowed efforts: {}",
                provider,
                model,
                effort.as_str(),
                allowed
                    .iter()
                    .map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Some(_) => Ok(()),
        }
    }
}

/// Context limits for a model, preserving the unit used by the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindowCapability {
    /// Token limit, when the provider boundary measures tokens.
    pub max_tokens: Option<usize>,
    /// Message limit, when a local generator measures retained messages.
    pub max_messages: Option<usize>,
    /// Evidence supporting the limit.
    pub provenance: CapabilityProvenance,
}

/// Maximum completion tokens accepted by the exact model/adapter pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputTokenLimitCapability {
    pub max_tokens: Option<usize>,
    pub provenance: CapabilityProvenance,
}

impl OutputTokenLimitCapability {
    pub fn unknown() -> Self {
        Self {
            max_tokens: None,
            provenance: CapabilityProvenance::Unknown,
        }
    }
}

/// Request/response protocol used by the provider adapter for this model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    AnthropicMessages,
    OpenAiChatCompletions,
    OpenAiChatGptResponsesLite,
    GeminiGenerateContent,
}

/// Known wire protocol and the evidence for that binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireProtocolCapability {
    pub protocol: Option<WireProtocol>,
    pub provenance: CapabilityProvenance,
}

impl WireProtocolCapability {
    fn unknown() -> Self {
        Self {
            protocol: None,
            provenance: CapabilityProvenance::Unknown,
        }
    }
}

impl ContextWindowCapability {
    /// Construct an unknown context window with no assumed default.
    pub fn unknown() -> Self {
        Self {
            max_tokens: None,
            max_messages: None,
            provenance: CapabilityProvenance::Unknown,
        }
    }
}

/// Capabilities of one exact provider/model pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Provider identity owning the model.
    pub provider: String,
    /// Exact provider model ID.
    pub model: String,
    /// Incremental text streaming.
    pub streaming: ModelFeature,
    /// Provider-native tool calls.
    pub tools: ModelFeature,
    /// Multiple tool calls returned in a single assistant turn.
    pub parallel_tool_calls: ModelFeature,
    /// Provider-enforced structured response schemas.
    pub structured_output: ModelFeature,
    /// Image parts accepted as model input.
    pub image_input: ModelFeature,
    /// Audio parts accepted as model input.
    pub audio_input: ModelFeature,
    /// Token/usage accounting returned through Finch's response boundary.
    pub usage_reporting: ModelFeature,
    /// Provider-scoped continuation handles.
    pub continuation: ModelFeature,
    /// Request-controlled reasoning effort.
    pub reasoning: ReasoningCapability,
    /// Context window and its unit.
    pub context_window: ContextWindowCapability,
    /// Maximum requested completion length, distinct from input context.
    pub output_token_limit: OutputTokenLimitCapability,
    /// Concrete transport contract used for this exact pair.
    pub wire_protocol: WireProtocolCapability,
}

impl ModelCapabilities {
    /// A fail-closed descriptor for an unrecognized model.
    pub fn unknown(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            streaming: ModelFeature::unknown(),
            tools: ModelFeature::unknown(),
            parallel_tool_calls: ModelFeature::unknown(),
            structured_output: ModelFeature::unknown(),
            image_input: ModelFeature::unknown(),
            audio_input: ModelFeature::unknown(),
            usage_reporting: ModelFeature::unknown(),
            continuation: ModelFeature::unknown(),
            reasoning: ReasoningCapability::unknown(),
            context_window: ContextWindowCapability::unknown(),
            output_token_limit: OutputTokenLimitCapability::unknown(),
            wire_protocol: WireProtocolCapability::unknown(),
        }
    }

    /// Build a descriptor from dated, provider-contract metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn static_metadata(
        provider: impl Into<String>,
        model: impl Into<String>,
        tested_on: &str,
        source: &str,
        streaming: CapabilitySupport,
        tools: CapabilitySupport,
        continuation: CapabilitySupport,
        reasoning: ReasoningCapability,
        max_tokens: Option<usize>,
        max_output_tokens: Option<usize>,
        max_messages: Option<usize>,
    ) -> Self {
        let feature = |support| ModelFeature::static_metadata(support, tested_on, source);
        Self {
            provider: provider.into(),
            model: model.into(),
            streaming: feature(streaming),
            tools: feature(tools),
            parallel_tool_calls: ModelFeature::unknown(),
            structured_output: ModelFeature::unknown(),
            image_input: ModelFeature::unknown(),
            audio_input: ModelFeature::unknown(),
            usage_reporting: ModelFeature::unknown(),
            continuation: feature(continuation),
            reasoning,
            context_window: ContextWindowCapability {
                max_tokens,
                max_messages,
                provenance: CapabilityProvenance::StaticMetadata {
                    tested_on: tested_on.to_string(),
                    source: source.to_string(),
                },
            },
            output_token_limit: if let Some(max_tokens) = max_output_tokens {
                OutputTokenLimitCapability {
                    max_tokens: Some(max_tokens),
                    provenance: CapabilityProvenance::StaticMetadata {
                        tested_on: tested_on.to_string(),
                        source: source.to_string(),
                    },
                }
            } else {
                OutputTokenLimitCapability::unknown()
            },
            wire_protocol: WireProtocolCapability::unknown(),
        }
    }

    /// Bind this exact model descriptor to the adapter's configured wire
    /// protocol without implying any unverified optional model features.
    pub fn with_wire_protocol(
        mut self,
        protocol: WireProtocol,
        tested_on: &str,
        source: &str,
    ) -> Self {
        self.wire_protocol = WireProtocolCapability {
            protocol: Some(protocol),
            provenance: CapabilityProvenance::StaticMetadata {
                tested_on: tested_on.to_string(),
                source: source.to_string(),
            },
        };
        self
    }

    fn require(&self, feature: &str, capability: &ModelFeature) -> anyhow::Result<()> {
        match capability.support {
            CapabilitySupport::Supported => Ok(()),
            CapabilitySupport::Unsupported => anyhow::bail!(
                "Provider '{}' model '{}' does not support {}",
                self.provider,
                self.model,
                feature
            ),
            CapabilitySupport::Unknown => anyhow::bail!(
                "Provider '{}' model '{}' has unknown {} capability; refusing to assume support",
                self.provider,
                self.model,
                feature
            ),
        }
    }

    /// Validate all request-local requirements before provider work begins.
    pub fn validate_request(
        &self,
        request: &ProviderRequest,
        streaming: bool,
        reasoning: Option<ReasoningEffort>,
    ) -> anyhow::Result<()> {
        if streaming || request.stream {
            self.require("streaming", &self.streaming)?;
        }
        if request
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty())
        {
            self.require("tool calls", &self.tools)?;
        }
        if request.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::OpaqueReasoning { .. }))
        }) {
            self.require("opaque provider continuation", &self.continuation)?;
        }
        if request.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Image { .. }))
        }) {
            self.require("image input", &self.image_input)?;
        }
        if let Some(effort) = reasoning {
            self.reasoning
                .require(&self.provider, &self.model, effort)?;
        }
        if let Some(limit) = self.output_token_limit.max_tokens {
            if request.max_tokens as usize > limit {
                anyhow::bail!(
                    "Provider '{}' model '{}' supports at most {} output tokens, but {} were requested",
                    self.provider,
                    self.model,
                    limit,
                    request.max_tokens
                );
            }
        }
        Ok(())
    }
}

/// Unified request format for all LLM providers
///
/// This wraps the existing Message format and adds provider-agnostic options.
/// Each provider implementation will transform this into their specific API format.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderRequest {
    /// Conversation messages (using Claude's Message format as the common denominator)
    pub messages: Vec<Message>,

    /// Model name (provider-specific)
    pub model: String,

    /// Maximum tokens to generate
    pub max_tokens: u32,

    /// System prompt (provider-specific handling: sent as `system` for Claude,
    /// prepended as a `{"role":"system"}` message for OpenAI-compatible providers)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,

    /// Tool definitions (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,

    /// Temperature (0.0 to 1.0, optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Whether to stream the response
    #[serde(skip)]
    pub stream: bool,

    /// Caller-owned cancellation propagated to network transports.
    #[serde(skip)]
    pub cancellation_token: Option<tokio_util::sync::CancellationToken>,
}

impl ProviderRequest {
    /// Create a new request from messages
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            model: String::new(), // Will be set by provider
            max_tokens: 4096,
            system: None,
            tools: None,
            temperature: None,
            stream: false,
            cancellation_token: None,
            cancellation_token: None,
        }
    }

    /// Set the model name
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set max tokens
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set system prompt
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Add tools to the request
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Enable streaming
    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    pub fn with_cancellation_token(
        mut self,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Self {
        self.cancellation_token = Some(cancellation_token);
        self
    }

    /// Set temperature
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Remove orphaned tool_use blocks from the end of the conversation.
    ///
    /// When a provider fails mid-agentic-loop (e.g. Grok 403), the conversation
    /// may end with an assistant message containing ToolUse blocks that have no
    /// corresponding ToolResult in the next message. Claude (and others) reject
    /// such histories with a 400 error. This method trims those orphaned tail
    /// messages so the fallback provider sees a clean conversation.
    pub fn sanitize_messages(&mut self) {
        use crate::claude::types::ContentBlock;

        loop {
            // Find the last assistant message index
            let last_assistant = self.messages.iter().rposition(|m| m.role == "assistant");
            let last_assistant_idx = match last_assistant {
                Some(i) => i,
                None => break,
            };

            // Check if it contains any ToolUse blocks
            let has_tool_use = self.messages[last_assistant_idx]
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }));

            if !has_tool_use {
                break;
            }

            // Check if the very next message has ToolResult blocks for all tool uses
            let tool_use_ids: Vec<String> = self.messages[last_assistant_idx]
                .content
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::ToolUse { id, .. } = b {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();

            let next_idx = last_assistant_idx + 1;
            let all_matched = if next_idx < self.messages.len() {
                tool_use_ids.iter().all(|id| {
                    self.messages[next_idx].content.iter().any(|b| {
                        matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == id)
                    })
                })
            } else {
                false
            };

            if all_matched {
                break;
            }

            // Orphaned — remove the assistant message (and any partial result after it)
            if next_idx < self.messages.len()
                && self.messages[next_idx].role == "user"
                && self.messages[next_idx]
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
            {
                self.messages.remove(next_idx);
            }
            self.messages.remove(last_assistant_idx);
        }

        // Forward scan: remove ToolResult blocks whose tool_use_id has no matching
        // ToolUse in the immediately preceding assistant message.
        //
        // This catches mid-conversation orphans that the tail-cleanup loop above
        // misses — e.g. when a user message ends up with a stale ToolResult from a
        // cancelled or retried round after the conversation history is rebuilt.
        {
            use std::collections::HashSet;
            let mut i = 0;
            while i < self.messages.len() {
                if self.messages[i].role != "user" {
                    i += 1;
                    continue;
                }

                // Collect the ToolUse IDs from the immediately preceding assistant message.
                let allowed_ids: HashSet<String> =
                    if i > 0 && self.messages[i - 1].role == "assistant" {
                        self.messages[i - 1]
                            .content
                            .iter()
                            .filter_map(|b| {
                                if let ContentBlock::ToolUse { id, .. } = b {
                                    Some(id.clone())
                                } else {
                                    None
                                }
                            })
                            .collect()
                    } else {
                        HashSet::new()
                    };

                // Retain only ToolResult blocks whose IDs are in allowed_ids;
                // non-ToolResult blocks are always kept.
                let had_tool_results = self.messages[i]
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
                self.messages[i].content.retain(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        allowed_ids.contains(tool_use_id)
                    }
                    _ => true,
                });

                // If the message is now empty (was all orphaned ToolResults), drop it.
                if self.messages[i].content.is_empty() && had_tool_results {
                    self.messages.remove(i);
                    continue;
                }

                i += 1;
            }
        }

        // Strip images that exceed provider limits (Claude: 5 MB, Grok: unsupported).
        // base64-encoded PNG from a Retina screenshot can easily be 7+ MB.
        // Replace them with a text placeholder so the conversation stays valid.
        const MAX_IMAGE_BASE64_BYTES: usize = 4_000_000; // ~3 MB raw — safe for all providers
        for msg in &mut self.messages {
            for block in &mut msg.content {
                if let ContentBlock::Image { source } = block {
                    if source.data.len() > MAX_IMAGE_BASE64_BYTES {
                        *block = ContentBlock::Text {
                            text: "[image omitted: too large to send]".to_string(),
                        };
                    }
                }
            }
        }
    }

    /// Truncate conversation history to fit within a provider's context window.
    ///
    /// Uses a conservative 3-chars-per-token heuristic (see `estimate_message_tokens`).
    /// Drops the oldest messages first, always preserving at least the last message.
    /// Returns the number of messages dropped.
    ///
    /// The budget is: `token_limit - system_prompt_tokens - max_tokens (response reserve)`
    pub fn truncate_to_context_limit(&mut self, token_limit: usize) -> usize {
        let system_tokens = self.system.as_deref().map(|s| s.len() / 3).unwrap_or(0);
        let response_reserve = self.max_tokens as usize;
        let budget = token_limit
            .saturating_sub(system_tokens)
            .saturating_sub(response_reserve);

        let costs: Vec<usize> = self.messages.iter().map(estimate_message_tokens).collect();
        let total: usize = costs.iter().sum();

        if total <= budget {
            return 0;
        }

        // Find the newest prefix we can include within budget (newest messages have priority)
        let mut running = 0usize;
        let mut keep_from = self.messages.len(); // messages[keep_from..] will be sent
        for i in (0..self.messages.len()).rev() {
            if running + costs[i] > budget {
                break;
            }
            running += costs[i];
            keep_from = i;
        }

        // Always keep at least the last message so we have something to send
        keep_from = keep_from.min(self.messages.len().saturating_sub(1));

        let dropped = keep_from;
        self.messages = self.messages.drain(keep_from..).collect();

        // After dropping from the head, the first message may now be a user
        // message containing only tool_result blocks whose corresponding
        // tool_use (in the preceding assistant message) was just dropped.
        // All providers reject tool_result without a matching tool_use, so
        // strip any such orphaned pairs from the front of the window.
        use crate::claude::types::ContentBlock;
        loop {
            if self.messages.len() <= 1 {
                break;
            }
            let first_is_orphaned = self
                .messages
                .first()
                .map(|m| {
                    m.role == "user"
                        && !m.content.is_empty()
                        && m.content
                            .iter()
                            .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
                })
                .unwrap_or(false);
            if !first_is_orphaned {
                break;
            }
            self.messages.remove(0); // drop orphaned tool_result user turn
                                     // Drop the assistant reply that answered it (if still present).
            if self.messages.first().map(|m| m.role.as_str()) == Some("assistant") {
                self.messages.remove(0);
            }
        }

        dropped
    }
}

/// Estimate the token cost of a single message.
///
/// Uses a 3-chars-per-token heuristic (conservative).  In practice, English prose
/// averages ~4 chars/token, but code and JSON tool-result content is denser
/// (closer to 3 chars/token).  Using 3 ensures we don't underestimate and
/// inadvertently send payloads that exceed provider context limits.
///
/// Adds a small per-message overhead for role and structural JSON tokens.
fn estimate_message_tokens(msg: &Message) -> usize {
    let content_chars: usize = msg
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
            ContentBlock::ToolResult { content, .. } => content.len(),
            ContentBlock::Image { source } => source.data.len().min(4_000) + 20,
            ContentBlock::OpaqueReasoning { encrypted_content } => encrypted_content.len(),
        })
        .sum();
    (content_chars / 3).max(1) + 4 // +4 overhead per message
}

/// Unified response format from LLM providers
///
/// This wraps the provider-specific response in a common format.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderResponse {
    /// Response ID (provider-specific)
    pub id: String,

    /// Model that generated the response
    pub model: String,

    /// Content blocks (text, tool_use, etc.)
    pub content: Vec<ContentBlock>,

    /// Why the model stopped generating
    pub stop_reason: Option<String>,

    /// Role of the responder (usually "assistant")
    pub role: String,

    /// Provider name (e.g., "claude", "openai", "gemini")
    pub provider: String,

    /// Provider-reported token counts, when supplied by the terminal response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ProviderUsage>,

    /// Subscription allowance metadata, distinct from API billing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowance: Option<ProviderAllowance>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ProviderAllowance {
    pub primary_used_percent: Option<f32>,
    pub secondary_used_percent: Option<f32>,
}

/// Provider-neutral identity and accounting for one completed inference.
///
/// Requested/resolved identity is Finch-owned dispatch state; `actual_model`
/// is authoritative provider response metadata. Subscription allowance is
/// deliberately distinct from billable token usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvocationMetadata {
    pub requested_model: String,
    pub resolved_model: String,
    pub actual_model: String,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub primary_allowance_used_percent: Option<f32>,
    pub secondary_allowance_used_percent: Option<f32>,
}

impl InvocationMetadata {
    pub fn validate(&self) -> anyhow::Result<()> {
        for model in [
            &self.requested_model,
            &self.resolved_model,
            &self.actual_model,
        ] {
            anyhow::ensure!(
                !model.is_empty() && model.len() <= 256 && model.is_ascii(),
                "provider invocation model identity was invalid"
            );
        }
        for allowance in [
            self.primary_allowance_used_percent,
            self.secondary_allowance_used_percent,
        ]
        .into_iter()
        .flatten()
        {
            anyhow::ensure!(
                allowance.is_finite() && (0.0..=100.0).contains(&allowance),
                "provider invocation allowance was invalid"
            );
        }
        Ok(())
    }
}

impl ProviderResponse {
    /// Extract text from the response
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| block.as_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Check if response contains tool uses
    pub fn has_tool_uses(&self) -> bool {
        self.content.iter().any(|block| block.is_tool_use())
    }

    /// Extract tool uses from response
    pub fn tool_uses(&self) -> Vec<crate::tools::types::ToolUse> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some(crate::tools::types::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Convert to Message for conversation history
    pub fn to_message(&self) -> Message {
        Message {
            role: self.role.clone(),
            content: self.content.clone(),
        }
    }
}

/// Stream chunk types for streaming responses
///
/// Re-export from generators module for convenience
pub use crate::generators::StreamChunk;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::Message;

    fn user_msg(text: &str) -> Message {
        Message::user(text)
    }

    #[test]
    fn test_provider_request_defaults() {
        let req = ProviderRequest::new(vec![user_msg("Hello")]);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.model, "");
        assert_eq!(req.max_tokens, 4096);
        assert!(req.tools.is_none());
        assert!(req.temperature.is_none());
        assert!(!req.stream);
    }

    #[test]
    fn test_provider_request_builder_chain() {
        let req = ProviderRequest::new(vec![user_msg("Hello")])
            .with_model("claude-sonnet-4-6")
            .with_max_tokens(1024)
            .with_temperature(0.7)
            .with_stream(true);

        assert_eq!(req.model, "claude-sonnet-4-6");
        assert_eq!(req.max_tokens, 1024);
        assert_eq!(req.temperature, Some(0.7));
        assert!(req.stream);
    }

    #[test]
    fn test_provider_request_with_model_string_conversions() {
        // with_model accepts &str and String
        let req1 = ProviderRequest::new(vec![]).with_model("gpt-4");
        let req2 = ProviderRequest::new(vec![]).with_model("gemini-pro".to_string());
        assert_eq!(req1.model, "gpt-4");
        assert_eq!(req2.model, "gemini-pro");
    }

    #[test]
    fn test_provider_request_multiple_messages() {
        let req = ProviderRequest::new(vec![
            Message::user("first"),
            Message::assistant("response"),
            Message::user("second"),
        ]);
        assert_eq!(req.messages.len(), 3);
    }

    // ─── sanitize_messages ───────────────────────────────────────────────────

    fn make_tool_use_msg(id: &str, name: &str) -> Message {
        Message::with_content(
            "assistant",
            vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: serde_json::json!({}),
            }],
        )
    }

    fn make_tool_result_msg(tool_use_id: &str, content: &str) -> Message {
        Message::with_content(
            "user",
            vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
                is_error: None,
            }],
        )
    }

    #[test]
    fn test_sanitize_empty_messages_is_noop() {
        let mut req = ProviderRequest::new(vec![]);
        req.sanitize_messages();
        assert!(req.messages.is_empty());
    }

    #[test]
    fn test_sanitize_normal_conversation_untouched() {
        let messages = vec![
            Message::user("hello"),
            Message::assistant("hi"),
            Message::user("how are you"),
        ];
        let mut req = ProviderRequest::new(messages.clone());
        req.sanitize_messages();
        assert_eq!(req.messages.len(), 3);
    }

    #[test]
    fn test_sanitize_removes_orphaned_tool_use() {
        // Assistant ends with tool_use but no tool_result follows
        let mut req = ProviderRequest::new(vec![
            Message::user("run ls"),
            make_tool_use_msg("call_1", "bash"),
        ]);
        req.sanitize_messages();
        // The orphaned assistant message should be removed
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
    }

    #[test]
    fn test_sanitize_keeps_matched_tool_use_and_result() {
        // Tool use followed by matching tool result — must stay
        let mut req = ProviderRequest::new(vec![
            Message::user("run ls"),
            make_tool_use_msg("call_1", "bash"),
            make_tool_result_msg("call_1", "file.txt"),
        ]);
        req.sanitize_messages();
        assert_eq!(req.messages.len(), 3);
    }

    #[test]
    fn test_sanitize_removes_partial_tool_results() {
        // Two tool_uses but only one has a result — orphaned pair removed
        let mut req = ProviderRequest::new(vec![
            Message::user("do work"),
            Message::with_content(
                "assistant",
                vec![
                    ContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "bash".to_string(),
                        input: serde_json::json!({}),
                    },
                    ContentBlock::ToolUse {
                        id: "call_2".to_string(),
                        name: "read".to_string(),
                        input: serde_json::json!({}),
                    },
                ],
            ),
            // Only one result — not all matched
            make_tool_result_msg("call_1", "done"),
        ]);
        req.sanitize_messages();
        // Both messages removed (incomplete pair)
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn test_sanitize_large_image_replaced_with_placeholder() {
        use crate::claude::types::ContentBlock;
        // Create a base64 string larger than 4 MB limit
        let large_data = "A".repeat(5_000_000);
        let mut req = ProviderRequest::new(vec![Message::with_content(
            "user",
            vec![ContentBlock::image("image/png", large_data)],
        )]);
        req.sanitize_messages();
        // Image block should be replaced with a text placeholder
        assert_eq!(req.messages.len(), 1);
        let block = &req.messages[0].content[0];
        match block {
            ContentBlock::Text { text } => assert!(text.contains("omitted")),
            _ => panic!("Expected image to be replaced by text block"),
        }
    }

    #[test]
    fn test_sanitize_small_image_kept() {
        use crate::claude::types::ContentBlock;
        let small_data = "iVBORw0KGgo="; // tiny valid base64
        let mut req = ProviderRequest::new(vec![Message::with_content(
            "user",
            vec![ContentBlock::image("image/png", small_data)],
        )]);
        req.sanitize_messages();
        // Small image kept as-is
        assert!(matches!(
            req.messages[0].content[0],
            ContentBlock::Image { .. }
        ));
    }

    // ─── truncate_to_context_limit ────────────────────────────────────────────

    #[test]
    fn test_truncate_noop_when_fits() {
        let mut req = ProviderRequest::new(vec![Message::user("hello"), Message::assistant("hi")]);
        let dropped = req.truncate_to_context_limit(200_000);
        assert_eq!(dropped, 0);
        assert_eq!(req.messages.len(), 2);
    }

    #[test]
    fn test_truncate_drops_oldest_messages() {
        // Create many large messages
        let big = "x".repeat(4_000); // ~1000 tokens each
        let mut req = ProviderRequest {
            messages: (0..50)
                .map(|i| {
                    if i % 2 == 0 {
                        Message::user(big.clone())
                    } else {
                        Message::assistant(big.clone())
                    }
                })
                .collect(),
            model: String::new(),
            max_tokens: 4096,
            system: None,
            tools: None,
            temperature: None,
            stream: false,
            cancellation_token: None,
        };
        let original_len = req.messages.len();
        let dropped = req.truncate_to_context_limit(10_000); // tight limit
        assert!(dropped > 0, "Should have dropped messages");
        assert_eq!(req.messages.len(), original_len - dropped);
        // Last message always preserved
        assert!(!req.messages.is_empty());
    }

    #[test]
    fn test_truncate_always_keeps_last_message() {
        // Single enormous message — should still be kept
        let big = "x".repeat(800_000); // way over any limit
        let mut req = ProviderRequest::new(vec![Message::user(big)]);
        let dropped = req.truncate_to_context_limit(1_000); // tiny limit
        assert_eq!(dropped, 0); // nothing to drop — only 1 message
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn test_truncate_counts_system_prompt_against_budget() {
        let system = "s".repeat(40_000); // ~13k tokens (40k chars / 3)
        let msg_text = "m".repeat(4_000); // ~1.3k tokens each (4k chars / 3)
        let mut req = ProviderRequest {
            messages: (0..10).map(|_| Message::user(msg_text.clone())).collect(),
            model: String::new(),
            max_tokens: 4096,
            system: Some(system),
            tools: None,
            temperature: None,
            stream: false,
        };
        // Limit of 20k tokens; ~13k system + ~4k response reserve = ~3k for messages
        // Each message ~1.3k tokens so only a couple fit → should drop several
        let dropped = req.truncate_to_context_limit(20_000);
        assert!(
            dropped > 0,
            "Should drop some messages due to system prompt cost"
        );
        assert!(!req.messages.is_empty());
    }

    // ─── ProviderResponse ─────────────────────────────────────────────────────

    fn make_response(content: Vec<ContentBlock>) -> ProviderResponse {
        ProviderResponse {
            id: "resp_1".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            content,
            stop_reason: Some("end_turn".to_string()),
            role: "assistant".to_string(),
            provider: "claude".to_string(),
            usage: None,
            allowance: None,
        }
    }

    #[test]
    fn test_provider_response_text_extracts_text_blocks() {
        let resp = make_response(vec![
            ContentBlock::Text {
                text: "Hello".to_string(),
            },
            ContentBlock::Text {
                text: " world".to_string(),
            },
        ]);
        let text = resp.text();
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
    }

    #[test]
    fn test_provider_response_text_skips_non_text() {
        let resp = make_response(vec![ContentBlock::ToolUse {
            id: "call_1".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({}),
        }]);
        assert_eq!(resp.text(), "");
    }

    #[test]
    fn test_provider_response_has_tool_uses() {
        let resp_with = make_response(vec![ContentBlock::ToolUse {
            id: "call_1".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
        }]);
        assert!(resp_with.has_tool_uses());

        let resp_without = make_response(vec![ContentBlock::Text {
            text: "done".to_string(),
        }]);
        assert!(!resp_without.has_tool_uses());
    }

    #[test]
    fn test_provider_response_tool_uses_extraction() {
        let resp = make_response(vec![
            ContentBlock::Text {
                text: "I'll run that".to_string(),
            },
            ContentBlock::ToolUse {
                id: "call_abc".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "pwd"}),
            },
        ]);
        let uses = resp.tool_uses();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].id, "call_abc");
        assert_eq!(uses[0].name, "bash");
    }

    #[test]
    fn test_provider_response_to_message() {
        let resp = make_response(vec![ContentBlock::Text {
            text: "hi".to_string(),
        }]);
        let msg = resp.to_message();
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn test_provider_request_system_prompt() {
        let req = ProviderRequest::new(vec![]).with_system("You are a helpful assistant.");
        assert_eq!(req.system.as_deref(), Some("You are a helpful assistant."));
    }

    #[test]
    fn test_provider_request_with_tools() {
        use crate::tools::types::{ToolDefinition, ToolInputSchema};
        let tool = ToolDefinition {
            name: "bash".to_string(),
            description: "Run commands".to_string(),
            input_schema: ToolInputSchema::simple(vec![("command", "Shell command")]),
        };
        let req = ProviderRequest::new(vec![]).with_tools(vec![tool]);
        assert!(req.tools.is_some());
        assert_eq!(req.tools.unwrap().len(), 1);
    }

    #[test]
    fn unknown_model_capabilities_remain_unknown() {
        let capabilities = ModelCapabilities::unknown("compatible", "unlisted-model");
        assert_eq!(capabilities.streaming.support, CapabilitySupport::Unknown);
        assert_eq!(capabilities.tools.support, CapabilitySupport::Unknown);
        assert_eq!(
            capabilities.parallel_tool_calls.support,
            CapabilitySupport::Unknown
        );
        assert_eq!(
            capabilities.structured_output.support,
            CapabilitySupport::Unknown
        );
        assert_eq!(capabilities.image_input.support, CapabilitySupport::Unknown);
        assert_eq!(capabilities.audio_input.support, CapabilitySupport::Unknown);
        assert_eq!(
            capabilities.usage_reporting.support,
            CapabilitySupport::Unknown
        );
        assert_eq!(
            capabilities.continuation.support,
            CapabilitySupport::Unknown
        );
        assert_eq!(capabilities.reasoning.support(), CapabilitySupport::Unknown);
        assert_eq!(capabilities.context_window.max_tokens, None);
        assert_eq!(capabilities.output_token_limit.max_tokens, None);
        assert_eq!(
            capabilities.context_window.provenance,
            CapabilityProvenance::Unknown
        );
        assert_eq!(
            capabilities.output_token_limit.provenance,
            CapabilityProvenance::Unknown
        );
        assert_eq!(capabilities.wire_protocol.protocol, None);
        assert_eq!(
            capabilities.wire_protocol.provenance,
            CapabilityProvenance::Unknown
        );
    }

    #[test]
    fn unknown_optional_capability_does_not_block_baseline_text() {
        let capabilities = ModelCapabilities::unknown("compatible", "unlisted-model");
        capabilities
            .validate_request(&ProviderRequest::new(vec![]), false, None)
            .unwrap();
        let error = capabilities
            .validate_request(&ProviderRequest::new(vec![]).with_stream(true), false, None)
            .unwrap_err();
        assert!(error.to_string().contains("unknown streaming capability"));
    }

    #[test]
    fn unsupported_requested_fields_have_precise_diagnostics() {
        let capabilities = ModelCapabilities::static_metadata(
            "text-only",
            "model-a",
            "2026-08-26",
            "test fixture",
            CapabilitySupport::Unsupported,
            CapabilitySupport::Unsupported,
            CapabilitySupport::Unsupported,
            ReasoningCapability::unsupported("2026-08-26", "test fixture"),
            Some(1_000),
            Some(10_000),
            None,
        );
        let tool = ToolDefinition {
            name: "lookup".into(),
            description: "lookup".into(),
            input_schema: crate::tools::types::ToolInputSchema::simple(vec![]),
        };
        let cases = [
            (
                ProviderRequest::new(vec![]).with_stream(true),
                "Provider 'text-only' model 'model-a' does not support streaming",
            ),
            (
                ProviderRequest::new(vec![]).with_tools(vec![tool]),
                "Provider 'text-only' model 'model-a' does not support tool calls",
            ),
        ];
        for (request, expected) in cases {
            assert_eq!(
                capabilities
                    .validate_request(&request, false, None)
                    .unwrap_err()
                    .to_string(),
                expected
            );
        }
    }
}
