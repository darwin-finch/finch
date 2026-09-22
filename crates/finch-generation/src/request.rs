//! Generation request, strategy, budget, and capabilities.

use crate::identity::BackendRef;
use finch_providers::{Message, ToolDefinition};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How a backend produces tokens over shared predictive state.
///
/// This is an interface tag, not a requirement that every backend implement
/// every strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationStrategy {
    /// Causal autoregressive decoding.
    CausalAutoregressive,
    /// Masked or diffusion-style refinement.
    MaskedRefinement,
    /// Direct (non-iterative) prediction.
    DirectPrediction,
    /// Hybrid of the above.
    Hybrid,
}

/// Matched information and resource budget for comparing backends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceBudget {
    /// Maximum completion tokens, when the backend measures tokens.
    pub max_output_tokens: Option<u32>,
    /// Optional timeout. `None` means the caller does not impose one.
    #[serde(with = "duration_millis_opt")]
    pub timeout: Option<Duration>,
}

impl ResourceBudget {
    /// Construct an unbounded budget.
    fn unlimited() -> Self {
        Self {
            max_output_tokens: None,
            timeout: None,
        }
    }

    /// True when two backends are being compared at the same budget.
    pub fn matches(&self, other: &Self) -> bool {
        self.max_output_tokens == other.max_output_tokens && self.timeout == other.timeout
    }
}

/// Features a backend is willing to claim. Unknown stays fail-closed at the
/// provider boundary; this struct is the generation-layer view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationCapabilities {
    /// Incremental token streaming.
    pub streaming: bool,
    /// Semantic tool calls.
    pub tools: bool,
    /// Multi-turn conversation.
    pub conversation: bool,
    /// Declared strategy.
    pub strategy: GenerationStrategy,
    /// Optional retained-message window for local generators.
    pub max_context_messages: Option<usize>,
}

impl GenerationCapabilities {
    /// Construct capabilities for a scripted or unknown backend.
    pub(crate) fn for_strategy(strategy: GenerationStrategy) -> Self {
        Self {
            streaming: true,
            tools: true,
            conversation: true,
            strategy,
            max_context_messages: None,
        }
    }
}

/// One generation attempt as seen by a backend.
#[derive(Debug, Clone)]
pub struct GenerationRequest {
    /// Conversation so far. Uses the provider-neutral `Message` type.
    pub messages: Vec<Message>,
    /// Tool schemas offered to the model. Never Finch `ToolExecutor` types.
    pub tools: Option<Vec<ToolDefinition>>,
    /// Caller-requested backend.
    pub requested: BackendRef,
    /// Strategy the caller wants this attempt to use.
    pub strategy: GenerationStrategy,
    /// Matched resource budget.
    pub budget: ResourceBudget,
    /// Caller-owned cancellation.
    pub cancellation: CancellationToken,
    /// When true, a supervisor may select another *named* candidate and must
    /// record why. Default false: no implicit cheaper-provider swap.
    pub allow_fallback: bool,
}

impl GenerationRequest {
    /// Construct a request with no tools, no timeout, and no fallback.
    pub fn new(
        messages: Vec<Message>,
        requested: BackendRef,
        strategy: GenerationStrategy,
    ) -> Self {
        Self {
            messages,
            tools: None,
            requested,
            strategy,
            budget: ResourceBudget::unlimited(),
            cancellation: CancellationToken::new(),
            allow_fallback: false,
        }
    }

    /// Attach a budget.
    pub fn with_budget(mut self, budget: ResourceBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Allow recorded fallback to another named candidate.
    pub fn with_fallback(mut self) -> Self {
        self.allow_fallback = true;
        self
    }

    /// Replace the cancellation token.
    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

mod duration_millis_opt {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(
        value: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .map(|duration| duration.as_millis() as u64)
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        let millis = Option::<u64>::deserialize(deserializer)?;
        Ok(millis.map(Duration::from_millis))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::BackendKind;

    #[test]
    fn test_resource_budget_matches_compares_tokens_and_timeout() {
        let a = ResourceBudget {
            max_output_tokens: Some(128),
            timeout: Some(Duration::from_millis(50)),
        };
        let b = ResourceBudget {
            max_output_tokens: Some(128),
            timeout: Some(Duration::from_millis(50)),
        };
        let c = ResourceBudget {
            max_output_tokens: Some(256),
            timeout: Some(Duration::from_millis(50)),
        };
        assert!(
            a.matches(&b),
            "identical token and timeout budgets must match: {a:?} vs {b:?}"
        );
        assert!(
            !a.matches(&c),
            "token mismatch must not compare as matched: {a:?} vs {c:?}"
        );
    }

    #[test]
    fn test_generation_request_defaults_forbid_implicit_fallback() {
        let requested = BackendRef::new("test", "script-1", BackendKind::Test).unwrap();
        let request = GenerationRequest::new(
            Vec::new(),
            requested,
            GenerationStrategy::CausalAutoregressive,
        );
        assert!(
            !request.allow_fallback,
            "fallback must be explicit; request={request:?}"
        );
    }
}
