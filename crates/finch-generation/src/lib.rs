//! Finch generation contract, lifecycle, and strategy ports.
//!
//! This crate owns the provider-neutral generation interface. Provider
//! transports live in `finch-providers`. Finch application types (Brain, TUI,
//! daemon, CLI, `Config`, `ToolExecutor`) stay outside. Child modules are
//! private; the `pub use` list is the public surface.

mod backend;
mod event;
mod identity;
mod ports;
mod provider;
mod readiness;
mod request;
mod scripted;
mod supervisor;
mod translate;

pub use backend::{select_backend, GenerationBackend};
pub use event::{
    Allowance, GenerationEvent, GenerationId, GenerationMetadata, ReasoningKind, TerminalOutcome,
    ToolCall, ToolResult, Usage,
};
pub use identity::{
    validate_model_id, BackendKind, BackendRef, GenerationIdentity, RejectedBackend, RouteDecision,
};
pub use ports::{
    ArtifactCache, BlockingScheduler, ControllableSleeper, EmptyCache, FrozenMonotonicClock,
    GenerationPorts, GenerationTelemetry, HardwareDiscovery, HardwareSnapshot, InlineScheduler,
    InstantSleeper, ModelLoader, MonotonicClock, ProgressSink, ReadyLoader, Sleeper,
    SystemMonotonicClock, TokioSleeper, TracingProgress, TracingTelemetry, UnknownHardware,
};
pub use provider::ProviderGenerationBackend;
pub use readiness::{LoadPhase, Readiness, ReadinessReport, ResourceMetadata};
pub use request::{GenerationCapabilities, GenerationRequest, GenerationStrategy, ResourceBudget};
pub use scripted::{ScriptedBackend, ScriptedStep};
pub use supervisor::GenerationSupervisor;
pub use translate::translate_provider_chunk;

pub use finch_providers::{
    ContentBlock, EventProvenance, Message, StreamChunk, ToolDefinition, ToolUse,
};

#[cfg(test)]
mod facade_tests {
    #[test]
    fn generation_facade_keeps_child_modules_private() {
        let facade = include_str!("lib.rs");
        let published = facade
            .lines()
            .map(str::trim_start)
            .filter(|line| !line.starts_with("//") && !line.starts_with("//!"))
            .filter(|line| line.starts_with("pub mod "))
            .collect::<Vec<_>>();
        assert!(
            published.is_empty(),
            "generation facade must keep child modules private; found: {published:?}"
        );
    }

    #[test]
    fn generation_crate_source_does_not_name_application_types() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let forbidden = [
            "crate::brain",
            "crate::cli",
            "crate::daemon",
            "crate::config::Config",
            "ToolExecutor",
            "ReplMode",
            "TuiRenderer",
        ];
        let mut hits = Vec::new();
        collect_forbidden(&root, &forbidden, &mut hits);
        assert!(
            hits.is_empty(),
            "finch-generation must not name Finch application types; found: {hits:?}"
        );
    }

    fn collect_forbidden(dir: &std::path::Path, forbidden: &[&str], hits: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_forbidden(&path, forbidden, hits);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (index, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with('"') {
                    continue;
                }
                for token in forbidden {
                    if line.contains(token) {
                        hits.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
                    }
                }
            }
        }
    }
}
