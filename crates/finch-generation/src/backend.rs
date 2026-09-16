//! Generation backend trait and candidate selection.

use crate::event::GenerationEvent;
use crate::identity::{BackendRef, RejectedBackend, RouteDecision};
use crate::readiness::{Readiness, ReadinessReport};
use crate::request::{GenerationCapabilities, GenerationRequest, GenerationStrategy};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

/// Shared generation contract for local, cloud, and test backends.
#[async_trait]
pub trait GenerationBackend: Send + Sync {
    /// Backend identity (provider + model + kind).
    fn identity(&self) -> BackendRef;

    /// Declared capabilities. Not conformance.
    fn capabilities(&self) -> GenerationCapabilities;

    /// Strategy this backend runs.
    fn strategy(&self) -> GenerationStrategy;

    /// Current readiness, load phase, and resource metadata.
    fn readiness(&self) -> ReadinessReport;

    /// Start one generation attempt. The returned stream ends with a
    /// [`GenerationEvent::Terminal`] or is closed, in which case the
    /// supervisor synthesizes a terminal.
    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<Receiver<Result<GenerationEvent>>>;
}

/// Select a backend for `request` among `candidates`.
///
/// Never auto-selects a cheaper provider. Fallback to another *named*
/// candidate happens only when `request.allow_fallback` is set, and the
/// decision records every rejection.
pub fn select_backend(
    request: &GenerationRequest,
    candidates: &[Arc<dyn GenerationBackend>],
) -> Result<(Arc<dyn GenerationBackend>, RouteDecision)> {
    let mut rejected = Vec::new();
    let mut exact: Option<Arc<dyn GenerationBackend>> = None;
    let mut fallback: Option<Arc<dyn GenerationBackend>> = None;

    for candidate in candidates {
        let identity = candidate.identity();
        let report = candidate.readiness();
        if identity.provider == request.requested.provider
            && identity.model == request.requested.model
        {
            if report.state != Readiness::Ready {
                rejected.push(RejectedBackend {
                    backend: identity,
                    reason: readiness_reason(&report),
                });
                continue;
            }
            exact = Some(Arc::clone(candidate));
            continue;
        }
        if report.state != Readiness::Ready {
            rejected.push(RejectedBackend {
                backend: identity,
                reason: readiness_reason(&report),
            });
            continue;
        }
        if fallback.is_none() {
            fallback = Some(Arc::clone(candidate));
        } else {
            rejected.push(RejectedBackend {
                backend: identity,
                reason: "not requested; another fallback candidate already recorded".into(),
            });
        }
    }

    if let Some(selected) = exact {
        let decision = RouteDecision {
            selected: selected.identity(),
            reason: "requested backend is ready".into(),
            rejected,
        };
        return Ok((selected, decision));
    }

    if request.allow_fallback {
        if let Some(selected) = fallback {
            rejected.retain(|entry| entry.backend != selected.identity());
            let decision = RouteDecision {
                selected: selected.identity(),
                reason: "requested backend unavailable; explicit fallback to named candidate"
                    .into(),
                rejected,
            };
            return Ok((selected, decision));
        }
    } else if let Some(candidate) = fallback {
        rejected.push(RejectedBackend {
            backend: candidate.identity(),
            reason: "not requested; implicit fallback is disabled".into(),
        });
    }

    let requested = request.requested.clone();
    anyhow::bail!(
        "no ready generation backend for provider '{}' model '{}'; rejected={}",
        requested.provider,
        requested.model,
        rejected.len()
    )
}

fn readiness_reason(report: &ReadinessReport) -> String {
    match report.state {
        Readiness::Ready => "ready".into(),
        Readiness::NotLoaded => "not loaded".into(),
        Readiness::Loading => format!(
            "loading ({})",
            report
                .phase
                .map(|phase| format!("{phase:?}"))
                .unwrap_or_else(|| "unknown phase".into())
        ),
        Readiness::Failed => report
            .failure_cause
            .clone()
            .unwrap_or_else(|| "load failed".into()),
    }
}
