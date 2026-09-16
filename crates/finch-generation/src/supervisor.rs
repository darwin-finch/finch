//! Generation supervisor: identity pinning, cancellation, timeout, switch fence.

use crate::backend::GenerationBackend;
use crate::event::{
    Allowance, GenerationEvent, GenerationId, GenerationMetadata, TerminalOutcome, Usage,
};
use crate::identity::{GenerationIdentity, RouteDecision};
use crate::ports::GenerationPorts;
use crate::readiness::Readiness;
use crate::request::GenerationRequest;
use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver};
use tokio_util::sync::CancellationToken;

/// Drives one generation attempt and drops events from superseded attempts.
#[derive(Clone)]
pub struct GenerationSupervisor {
    ports: GenerationPorts,
    current: Arc<Mutex<Option<GenerationId>>>,
    in_flight: Arc<Mutex<Option<CancellationToken>>>,
}

impl GenerationSupervisor {
    /// Construct a supervisor with injected ports.
    pub fn new(ports: GenerationPorts) -> Self {
        Self {
            ports,
            current: Arc::new(Mutex::new(None)),
            in_flight: Arc::new(Mutex::new(None)),
        }
    }

    /// Run `backend` for `request`. Emits `Started`, optional `Route`,
    /// backend events, and exactly one `Terminal`.
    pub async fn run(
        &self,
        backend: Arc<dyn GenerationBackend>,
        request: GenerationRequest,
        route: Option<RouteDecision>,
    ) -> Result<Receiver<Result<GenerationEvent>>> {
        let id = GenerationId::new();
        *self.current.lock().expect("current lock") = Some(id);
        self.spawn_attempt(id, backend, request, route)
    }

    /// Supersede the in-flight attempt and start `backend`. Events from the
    /// previous generation id are dropped.
    pub async fn switch(
        &self,
        backend: Arc<dyn GenerationBackend>,
        request: GenerationRequest,
        route: RouteDecision,
    ) -> Result<Receiver<Result<GenerationEvent>>> {
        let id = GenerationId::new();
        *self.current.lock().expect("current lock") = Some(id);
        self.spawn_attempt(id, backend, request, Some(route))
    }

    fn spawn_attempt(
        &self,
        id: GenerationId,
        backend: Arc<dyn GenerationBackend>,
        request: GenerationRequest,
        route: Option<RouteDecision>,
    ) -> Result<Receiver<Result<GenerationEvent>>> {
        {
            let mut in_flight = self.in_flight.lock().expect("in_flight lock");
            if let Some(previous) = in_flight.take() {
                previous.cancel();
            }
            *in_flight = Some(request.cancellation.clone());
        }
        let (tx, rx) = mpsc::channel(64);
        let ports = self.ports.clone();
        let current = Arc::clone(&self.current);
        tokio::spawn(async move {
            let identity =
                GenerationIdentity::for_dispatch(request.requested.clone(), backend.identity());
            let started_ms = ports.clock.now_ms();

            if !emit(
                &tx,
                &current,
                id,
                Ok(GenerationEvent::Started {
                    id,
                    identity: identity.clone(),
                }),
            )
            .await
            {
                return;
            }
            if let Some(route) = route {
                if !emit(&tx, &current, id, Ok(GenerationEvent::Route(route))).await {
                    return;
                }
            }

            let report = backend.readiness();
            if !emit(
                &tx,
                &current,
                id,
                Ok(GenerationEvent::Readiness(report.clone())),
            )
            .await
            {
                return;
            }
            if report.state != Readiness::Ready {
                let cause = report
                    .failure_cause
                    .clone()
                    .unwrap_or_else(|| format!("backend not ready ({:?})", report.state));
                let _ = emit(
                    &tx,
                    &current,
                    id,
                    Ok(GenerationEvent::Terminal(TerminalOutcome::Failed {
                        cause,
                        identity,
                    })),
                )
                .await;
                return;
            }

            let stream = match backend.generate(request.clone()).await {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = emit(
                        &tx,
                        &current,
                        id,
                        Ok(GenerationEvent::Terminal(TerminalOutcome::Failed {
                            cause: error.to_string(),
                            identity,
                        })),
                    )
                    .await;
                    return;
                }
            };

            drive_stream(
                stream, tx, current, id, identity, request, ports, started_ms,
            )
            .await;
        });
        Ok(rx)
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive_stream(
    mut stream: Receiver<Result<GenerationEvent>>,
    tx: mpsc::Sender<Result<GenerationEvent>>,
    current: Arc<Mutex<Option<GenerationId>>>,
    id: GenerationId,
    mut identity: GenerationIdentity,
    request: GenerationRequest,
    ports: GenerationPorts,
    started_ms: u64,
) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut usage = Usage::default();
    let mut allowance = Allowance::default();
    let mut stop_reason = None;
    let mut terminated = false;

    let timeout = request.budget.timeout;
    let cancel = request.cancellation.clone();

    loop {
        let timeout_sleep = timeout_future(timeout, &ports);

        tokio::select! {
            _ = cancel.cancelled() => {
                terminated = emit_terminal(
                    &tx,
                    &current,
                    id,
                    TerminalOutcome::Cancelled { identity: identity.clone() },
                )
                .await;
                break;
            }
            _ = timeout_sleep => {
                terminated = emit_terminal(
                    &tx,
                    &current,
                    id,
                    TerminalOutcome::TimedOut { identity: identity.clone() },
                )
                .await;
                break;
            }
            next = stream.recv() => {
                match next {
                    None => break,
                    Some(Err(error)) => {
                        terminated = emit_terminal(
                            &tx,
                            &current,
                            id,
                            TerminalOutcome::Failed {
                                cause: error.to_string(),
                                identity: identity.clone(),
                            },
                        )
                        .await;
                        break;
                    }
                    Some(Ok(event)) => {
                        if let GenerationEvent::Identity(updated) = &event {
                            identity = updated.clone();
                        }
                        if let GenerationEvent::TextDelta { text: delta, .. } = &event {
                            text.push_str(delta);
                        }
                        if let GenerationEvent::ToolCallComplete(call) = &event {
                            tool_calls.push(call.clone());
                        }
                        if let GenerationEvent::Usage(next_usage) = &event {
                            usage = next_usage.clone();
                        }
                        if let GenerationEvent::Allowance(next_allowance) = &event {
                            allowance = next_allowance.clone();
                        }
                        if let GenerationEvent::Terminal(outcome) = &event {
                            stop_reason = match outcome {
                                TerminalOutcome::Completed { metadata, .. } => {
                                    metadata.stop_reason.clone()
                                }
                                _ => None,
                            };
                            identity = outcome.identity().clone();
                            terminated = emit_terminal(&tx, &current, id, outcome.clone()).await;
                            break;
                        }
                        if !emit(&tx, &current, id, Ok(event)).await {
                            return;
                        }
                    }
                }
            }
        }
    }

    if terminated || !is_current(&current, id) {
        return;
    }
    let latency_ms = Some(ports.clock.now_ms().saturating_sub(started_ms));
    let _ = emit_terminal(
        &tx,
        &current,
        id,
        TerminalOutcome::Completed {
            text,
            tool_calls,
            metadata: GenerationMetadata {
                identity,
                usage,
                allowance,
                latency_ms,
                resources: crate::readiness::ResourceMetadata {
                    elapsed_ms: latency_ms.unwrap_or(0),
                    peak_memory_bytes: None,
                    accelerator: Some(ports.hardware.snapshot().accelerator),
                },
                stop_reason,
            },
        },
    )
    .await;
}

async fn timeout_future(timeout: Option<Duration>, ports: &GenerationPorts) {
    match timeout {
        Some(duration) => ports.sleeper.sleep(duration).await,
        None => std::future::pending().await,
    }
}

fn is_current(current: &Mutex<Option<GenerationId>>, id: GenerationId) -> bool {
    *current.lock().expect("current lock") == Some(id)
}

async fn emit(
    tx: &mpsc::Sender<Result<GenerationEvent>>,
    current: &Mutex<Option<GenerationId>>,
    id: GenerationId,
    event: Result<GenerationEvent>,
) -> bool {
    if !is_current(current, id) {
        return false;
    }
    tx.send(event).await.is_ok()
}

async fn emit_terminal(
    tx: &mpsc::Sender<Result<GenerationEvent>>,
    current: &Mutex<Option<GenerationId>>,
    id: GenerationId,
    outcome: TerminalOutcome,
) -> bool {
    let sent = emit(tx, current, id, Ok(GenerationEvent::Terminal(outcome))).await;
    if sent {
        let mut guard = current.lock().expect("current lock");
        if *guard == Some(id) {
            *guard = None;
        }
    }
    sent
}
