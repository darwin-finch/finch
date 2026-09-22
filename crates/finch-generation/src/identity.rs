//! Authoritative provider/model identity and routing provenance.

use anyhow::Result;
use serde::{Deserialize, Serialize};

const MAX_IDENTITY_BYTES: usize = 256;

/// Kind of generation backend. Not a billing or cost signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Cloud provider transport (`finch-providers`).
    Cloud,
    /// Local in-process inference.
    Local,
    /// Deterministic test double.
    Test,
}

/// One provider/model pair as named by a caller, router, or completed run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendRef {
    /// Provider identity (`"claude"`, `"openai"`, `"local"`, `"test"`).
    pub provider: String,
    /// Exact model id. Must pass [`validate_model_id`].
    pub model: String,
    /// How this backend is hosted.
    pub kind: BackendKind,
}

impl BackendRef {
    /// Construct and validate a backend reference.
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        kind: BackendKind,
    ) -> Result<Self> {
        let provider = provider.into();
        let model = model.into();
        validate_model_id(&provider)?;
        validate_model_id(&model)?;
        Ok(Self {
            provider,
            model,
            kind,
        })
    }
}

/// Requested versus resolved versus actual backend for one generation.
///
/// Requested is caller intent. Resolved is the routing decision. Actual is
/// what produced tokens. All three must remain secret-free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationIdentity {
    /// What the caller asked for.
    pub requested: BackendRef,
    /// What routing selected.
    pub resolved: BackendRef,
    /// What actually ran. Updated when a serving model is reported.
    pub actual: BackendRef,
}

impl GenerationIdentity {
    /// Pin requested = resolved = actual at the start of an attempt.
    pub fn pinned(backend: BackendRef) -> Self {
        Self {
            requested: backend.clone(),
            resolved: backend.clone(),
            actual: backend,
        }
    }

    /// Identity for one dispatch.
    ///
    /// Requested is caller intent. When this backend is the requested
    /// provider, resolved/actual follow the requested model (what is sent
    /// on the wire), not the backend's default. Explicit fallback to a
    /// different provider keeps the selected backend's identity.
    pub fn for_dispatch(requested: BackendRef, backend: BackendRef) -> Self {
        let resolved = if requested.provider == backend.provider {
            BackendRef {
                provider: backend.provider,
                model: requested.model.clone(),
                kind: backend.kind,
            }
        } else {
            backend
        };
        Self {
            requested,
            resolved: resolved.clone(),
            actual: resolved,
        }
    }

    /// Record a serving-model correction without changing requested/resolved.
    pub fn with_actual_model(mut self, model: impl Into<String>) -> Result<Self> {
        let model = model.into();
        validate_model_id(&model)?;
        self.actual.model = model;
        Ok(self)
    }
}

/// Why a candidate was not selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedBackend {
    /// Candidate that was not used.
    pub backend: BackendRef,
    /// Secret-free reason the candidate was rejected.
    pub reason: String,
}

/// Recorded routing decision. Never an automatic cheaper-provider swap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteDecision {
    /// Backend chosen for this attempt.
    pub selected: BackendRef,
    /// Why it was chosen.
    pub reason: String,
    /// Candidates considered and declined.
    pub rejected: Vec<RejectedBackend>,
}

/// Reject empty, oversized, non-graphic, or non-ASCII identity strings
/// without echoing the value into the error.
pub fn validate_model_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_IDENTITY_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        anyhow::bail!("generation model identity was invalid");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_model_id_accepts_bounded_ascii_graphic() {
        validate_model_id("claude-sonnet-4").unwrap();
        validate_model_id(&"m".repeat(MAX_IDENTITY_BYTES)).unwrap();
    }

    #[test]
    fn test_validate_model_id_rejects_without_echoing_the_value() {
        for model in [
            String::new(),
            format!("OVERSIZED{}", "m".repeat(MAX_IDENTITY_BYTES)),
            "CONTROL\nINJECTED".to_string(),
            "SPACE INJECTED".to_string(),
            "unicode_mødël".to_string(),
        ] {
            let error = validate_model_id(&model).unwrap_err().to_string();
            assert_eq!(
                error, "generation model identity was invalid",
                "invalid identity must use a fixed secret-free error; got {error:?} for a rejected value"
            );
            if !model.is_empty() {
                assert!(
                    !error.contains(&model),
                    "identity error must not echo the rejected value {model:?}: {error}"
                );
            }
        }
    }

    #[test]
    fn test_generation_identity_with_actual_model_preserves_requested() {
        let requested = BackendRef::new("claude", "claude-sonnet-4", BackendKind::Cloud).unwrap();
        let identity = GenerationIdentity::pinned(requested.clone())
            .with_actual_model("claude-sonnet-4-served")
            .unwrap();
        assert_eq!(identity.requested, requested);
        assert_eq!(identity.resolved.model, "claude-sonnet-4");
        assert_eq!(identity.actual.model, "claude-sonnet-4-served");
    }

    #[test]
    fn test_for_dispatch_follows_requested_model_not_backend_default() {
        let requested = BackendRef::new("claude", "requested-model", BackendKind::Cloud).unwrap();
        let backend = BackendRef::new("claude", "default-model", BackendKind::Cloud).unwrap();
        let identity = GenerationIdentity::for_dispatch(requested.clone(), backend);
        assert_eq!(identity.requested.model, "requested-model");
        assert_eq!(
            identity.resolved.model, "requested-model",
            "same-provider dispatch must resolve the requested model, not the default: {identity:?}"
        );
        assert_eq!(identity.actual.model, "requested-model");
        assert_ne!(identity.resolved.model, "default-model");
    }

    #[test]
    fn test_for_dispatch_keeps_fallback_backend_identity() {
        let requested = BackendRef::new("local", "missing", BackendKind::Local).unwrap();
        let backend = BackendRef::new("claude", "cloud-primary", BackendKind::Cloud).unwrap();
        let identity = GenerationIdentity::for_dispatch(requested.clone(), backend.clone());
        assert_eq!(identity.requested, requested);
        assert_eq!(
            identity.resolved, backend,
            "fallback to another provider must keep that backend's identity: {identity:?}"
        );
        assert_eq!(identity.actual, backend);
    }
}
