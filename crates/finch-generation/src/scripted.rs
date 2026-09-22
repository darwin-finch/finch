//! Deterministic local, cloud, and refinement backends for tests.

use crate::backend::GenerationBackend;
use crate::event::GenerationEvent;
use crate::identity::BackendRef;
use crate::readiness::{LoadPhase, Readiness, ReadinessReport, ResourceMetadata};
use crate::request::{GenerationCapabilities, GenerationRequest, GenerationStrategy};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{self, Receiver};
use tokio::sync::Notify;

/// One step in a scripted generation.
#[derive(Clone)]
pub enum ScriptedStep {
    /// Emit an event.
    Event(GenerationEvent),
    /// Park until `notify` is signalled. Used for late-completion tests.
    Wait(Arc<Notify>),
    /// Fail the stream.
    Fail(String),
}

/// Scripted backend used by production-boundary tests.
pub struct ScriptedBackend {
    identity: BackendRef,
    strategy: GenerationStrategy,
    capabilities: GenerationCapabilities,
    readiness: Mutex<ReadinessReport>,
    script: Mutex<Vec<ScriptedStep>>,
}

impl ScriptedBackend {
    /// Construct a ready backend that plays `script`.
    pub fn ready(
        identity: BackendRef,
        strategy: GenerationStrategy,
        script: Vec<ScriptedStep>,
    ) -> Self {
        Self {
            identity,
            strategy,
            capabilities: GenerationCapabilities::for_strategy(strategy),
            readiness: Mutex::new(ReadinessReport::ready(
                std::time::Duration::ZERO,
                ResourceMetadata::default(),
            )),
            script: Mutex::new(script),
        }
    }

    /// Construct a backend in `report` that plays `script` once ready.
    pub fn with_readiness(
        identity: BackendRef,
        strategy: GenerationStrategy,
        report: ReadinessReport,
        script: Vec<ScriptedStep>,
    ) -> Self {
        Self {
            identity,
            strategy,
            capabilities: GenerationCapabilities::for_strategy(strategy),
            readiness: Mutex::new(report),
            script: Mutex::new(script),
        }
    }

    /// Replace the readiness report (background-load tests).
    fn set_readiness(&self, report: ReadinessReport) {
        *self.readiness.lock().expect("readiness lock") = report;
    }

    /// Advance a loading backend through `phase`.
    pub fn set_phase(&self, phase: LoadPhase, elapsed_ms: u64) {
        let state = if phase == LoadPhase::Ready {
            Readiness::Ready
        } else {
            Readiness::Loading
        };
        self.set_readiness(ReadinessReport {
            state,
            phase: Some(phase),
            elapsed: std::time::Duration::from_millis(elapsed_ms),
            failure_cause: None,
            resources: ResourceMetadata {
                elapsed_ms,
                peak_memory_bytes: None,
                accelerator: Some("cpu".into()),
            },
        });
    }
}

#[async_trait]
impl GenerationBackend for ScriptedBackend {
    fn identity(&self) -> BackendRef {
        self.identity.clone()
    }

    fn capabilities(&self) -> GenerationCapabilities {
        self.capabilities.clone()
    }

    fn strategy(&self) -> GenerationStrategy {
        self.strategy
    }

    fn readiness(&self) -> ReadinessReport {
        self.readiness.lock().expect("readiness lock").clone()
    }

    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<Receiver<Result<GenerationEvent>>> {
        let report = self.readiness();
        if report.state != Readiness::Ready {
            anyhow::bail!(
                "backend '{}' model '{}' is not ready ({:?})",
                self.identity.provider,
                self.identity.model,
                report.state
            );
        }
        if request.strategy != self.strategy {
            anyhow::bail!(
                "backend '{}' does not run strategy {:?}",
                self.identity.provider,
                request.strategy
            );
        }
        let script = self.script.lock().expect("script lock").clone();
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move {
            for step in script {
                if request.cancellation.is_cancelled() {
                    break;
                }
                match step {
                    ScriptedStep::Event(event) => {
                        if tx.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                    ScriptedStep::Wait(notify) => {
                        notify.notified().await;
                    }
                    ScriptedStep::Fail(cause) => {
                        let _ = tx.send(Err(anyhow::anyhow!(cause))).await;
                        return;
                    }
                }
            }
        });
        Ok(rx)
    }
}
