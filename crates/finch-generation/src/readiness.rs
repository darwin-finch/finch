//! Model loading phases, readiness, and secret-free resource metadata.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Whether a backend can accept a generate call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    /// Weights are not present.
    NotLoaded,
    /// A load is in flight.
    Loading,
    /// Generate calls may proceed.
    Ready,
    /// Load failed; [`ReadinessReport::failure_cause`] names why.
    Failed,
}

/// Explicit phase of a model load. Not proof of conformance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadPhase {
    /// Hardware probe.
    DiscoveringHardware,
    /// Fetching artifacts.
    Downloading,
    /// Reading from cache.
    Caching,
    /// Mapping weights into memory.
    LoadingWeights,
    /// Optional warmup.
    Warming,
    /// Ready to generate.
    Ready,
}

/// Secret-free resource snapshot for a load or generate attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ResourceMetadata {
    /// Elapsed time in milliseconds from the injected clock.
    pub elapsed_ms: u64,
    /// Peak memory when the host reports it.
    pub peak_memory_bytes: Option<u64>,
    /// Hardware class (`"cpu"`, `"metal"`, `"cuda"`, `"unknown"`).
    pub accelerator: Option<String>,
}

/// Readiness plus the load story that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessReport {
    /// Current readiness.
    pub state: Readiness,
    /// Phase when loading or ready.
    pub phase: Option<LoadPhase>,
    /// Elapsed time for the current load or since ready.
    pub elapsed: Duration,
    /// Secret-free failure cause when [`Readiness::Failed`].
    pub failure_cause: Option<String>,
    /// Resource metadata.
    pub resources: ResourceMetadata,
}

impl ReadinessReport {
    /// Construct a ready report with no load failure.
    pub fn ready(elapsed: Duration, resources: ResourceMetadata) -> Self {
        Self {
            state: Readiness::Ready,
            phase: Some(LoadPhase::Ready),
            elapsed,
            failure_cause: None,
            resources,
        }
    }

    /// Construct a loading report for `phase`.
    pub fn loading(phase: LoadPhase, elapsed: Duration, resources: ResourceMetadata) -> Self {
        Self {
            state: Readiness::Loading,
            phase: Some(phase),
            elapsed,
            failure_cause: None,
            resources,
        }
    }

    /// Construct a failed report. `cause` must not contain secrets.
    pub fn failed(
        cause: impl Into<String>,
        elapsed: Duration,
        resources: ResourceMetadata,
    ) -> Self {
        Self {
            state: Readiness::Failed,
            phase: None,
            elapsed,
            failure_cause: Some(cause.into()),
            resources,
        }
    }

    /// Construct a not-loaded report.
    pub fn not_loaded() -> Self {
        Self {
            state: Readiness::NotLoaded,
            phase: None,
            elapsed: Duration::ZERO,
            failure_cause: None,
            resources: ResourceMetadata::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_readiness_report_failed_preserves_cause_and_elapsed() {
        let report = ReadinessReport::failed(
            "weights missing",
            Duration::from_millis(40),
            ResourceMetadata {
                elapsed_ms: 40,
                peak_memory_bytes: None,
                accelerator: Some("cpu".into()),
            },
        );
        assert_eq!(report.state, Readiness::Failed);
        assert_eq!(report.failure_cause.as_deref(), Some("weights missing"));
        assert_eq!(report.elapsed, Duration::from_millis(40));
        assert_eq!(report.resources.accelerator.as_deref(), Some("cpu"));
    }
}
