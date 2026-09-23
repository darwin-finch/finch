//! Production-boundary lifecycle tests for the generation contract.

use finch_generation::{
    select_backend, translate_provider_chunk, Allowance, BackendKind, BackendRef,
    ControllableSleeper, FrozenMonotonicClock, GenerationBackend, GenerationEvent, GenerationPorts,
    GenerationRequest, GenerationStrategy, GenerationSupervisor, LoadPhase,
    ProviderGenerationBackend, ReadinessReport, ResourceBudget, ResourceMetadata, ScriptedBackend,
    ScriptedStep, TerminalOutcome, ToolCall,
};
use finch_providers::{
    CapabilitySupport, ContentBlock, EventProvenance, ModelCapabilities, ProviderBackend,
    ProviderResponse, ReasoningCapability, StreamChunk, ValidatedProviderRequest,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

fn test_ref(provider: &str, model: &str) -> BackendRef {
    BackendRef::new(provider, model, BackendKind::Test).unwrap()
}

fn provenance(backend: &BackendRef, event: &str, sequence: u64) -> EventProvenance {
    EventProvenance {
        provider: backend.provider.clone(),
        model: backend.model.clone(),
        event: event.into(),
        sequence,
        opaque_replay: None,
    }
}

fn text_delta(backend: &BackendRef, text: &str, sequence: u64) -> ScriptedStep {
    ScriptedStep::Event(GenerationEvent::TextDelta {
        text: text.into(),
        provenance: provenance(backend, "text", sequence),
    })
}

async fn collect(
    mut rx: tokio::sync::mpsc::Receiver<anyhow::Result<GenerationEvent>>,
) -> Vec<GenerationEvent> {
    let mut events = Vec::new();
    while let Some(item) = rx.recv().await {
        events.push(item.expect("generation stream error"));
    }
    events
}

async fn drain_until_started(
    rx: &mut tokio::sync::mpsc::Receiver<anyhow::Result<GenerationEvent>>,
) -> Vec<GenerationEvent> {
    let mut events = Vec::new();
    while let Some(item) = rx.recv().await {
        let event = item.expect("generation stream error before Started");
        let started = matches!(event, GenerationEvent::Started { .. });
        events.push(event);
        if started {
            break;
        }
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GenerationEvent::Started { .. })),
        "supervisor never emitted Started; events={events:?}"
    );
    events
}

fn terminals(events: &[GenerationEvent]) -> Vec<&TerminalOutcome> {
    events
        .iter()
        .filter_map(|event| match event {
            GenerationEvent::Terminal(outcome) => Some(outcome),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn test_supervisor_emits_exactly_one_terminal_for_completed_run() {
    let backend_ref = test_ref("local", "qwen-test");
    let backend = Arc::new(ScriptedBackend::ready(
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
        vec![text_delta(&backend_ref, "hello", 1)],
    ));
    let supervisor = GenerationSupervisor::new(GenerationPorts::test());
    let request = GenerationRequest::new(
        vec![finch_providers::Message::user("hi")],
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
    );
    let events = collect(supervisor.run(backend, request, None).await.unwrap()).await;
    let terms = terminals(&events);
    assert_eq!(
        terms.len(),
        1,
        "completed run must terminalize exactly once; events={events:?}"
    );
    match terms[0] {
        TerminalOutcome::Completed { text, metadata, .. } => {
            assert_eq!(text, "hello");
            assert_eq!(metadata.identity.actual, backend_ref);
            assert_eq!(metadata.identity.requested, backend_ref);
        }
        other => panic!("expected Completed, got {other:?} from events {events:?}"),
    }
}

#[tokio::test]
async fn test_background_loading_reports_phase_elapsed_and_blocks_generate() {
    let backend_ref = test_ref("local", "loading-model");
    let backend = Arc::new(ScriptedBackend::with_readiness(
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
        ReadinessReport::loading(
            LoadPhase::LoadingWeights,
            Duration::from_millis(12),
            ResourceMetadata {
                elapsed_ms: 12,
                peak_memory_bytes: Some(64 * 1024 * 1024),
                accelerator: Some("cpu".into()),
            },
        ),
        vec![text_delta(&backend_ref, "should not run", 1)],
    ));
    let supervisor = GenerationSupervisor::new(GenerationPorts::test());
    let request = GenerationRequest::new(
        Vec::new(),
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
    );
    let events = collect(
        supervisor
            .run(backend.clone(), request, None)
            .await
            .unwrap(),
    )
    .await;
    let readiness = events.iter().find_map(|event| match event {
        GenerationEvent::Readiness(report) => Some(report),
        _ => None,
    });
    let report = readiness.expect(&format!("missing readiness event: {events:?}"));
    assert_eq!(report.phase, Some(LoadPhase::LoadingWeights));
    assert_eq!(report.elapsed, Duration::from_millis(12));
    assert_eq!(report.resources.elapsed_ms, 12);
    match terminals(&events).as_slice() {
        [TerminalOutcome::Failed { cause, identity }] => {
            assert!(
                cause.contains("not ready"),
                "loading backend must fail closed; cause={cause} events={events:?}"
            );
            assert_eq!(identity.actual, backend_ref);
        }
        other => panic!("expected Failed terminal, got {other:?} from {events:?}"),
    }

    backend.set_phase(LoadPhase::Ready, 40);
    let request = GenerationRequest::new(
        Vec::new(),
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
    );
    let ready_events = collect(supervisor.run(backend, request, None).await.unwrap()).await;
    match terminals(&ready_events).as_slice() {
        [TerminalOutcome::Completed { text, .. }] => assert_eq!(text, "should not run"),
        other => panic!("ready backend must complete; got {other:?} from {ready_events:?}"),
    }
}

#[tokio::test]
async fn test_cancellation_beats_late_completion() {
    let backend_ref = test_ref("cloud", "held");
    let hold = Arc::new(Notify::new());
    let backend = Arc::new(ScriptedBackend::ready(
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
        vec![
            ScriptedStep::Wait(Arc::clone(&hold)),
            text_delta(&backend_ref, "late", 1),
        ],
    ));
    let cancel = CancellationToken::new();
    let supervisor = GenerationSupervisor::new(GenerationPorts::test());
    let request = GenerationRequest::new(
        Vec::new(),
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
    )
    .with_cancellation(cancel.clone());
    let mut rx = supervisor.run(backend, request, None).await.unwrap();
    let mut events = drain_until_started(&mut rx).await;
    cancel.cancel();
    hold.notify_one();
    events.extend(collect(rx).await);
    match terminals(&events).as_slice() {
        [TerminalOutcome::Cancelled { identity }] => {
            assert_eq!(identity.actual.provider, "cloud");
            assert_eq!(identity.actual.model, "held");
        }
        other => panic!("expected Cancelled, got {other:?} from {events:?}"),
    }
    assert!(
        events.iter().all(|event| !matches!(
            event,
            GenerationEvent::TextDelta { text, .. } if text == "late"
        )),
        "cancelled attempt must drop late text after it is offered; events={events:?}"
    );
}

#[tokio::test]
async fn test_timeout_uses_injected_sleeper_not_wall_clock() {
    let backend_ref = test_ref("cloud", "slow");
    let hold = Arc::new(Notify::new());
    let backend = Arc::new(ScriptedBackend::ready(
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
        vec![
            ScriptedStep::Wait(Arc::clone(&hold)),
            text_delta(&backend_ref, "too late", 1),
        ],
    ));
    let sleeper = Arc::new(ControllableSleeper::new());
    let mut ports = GenerationPorts::test();
    ports.sleeper = sleeper.clone();
    ports.clock = Arc::new(FrozenMonotonicClock::new(0));
    let supervisor = GenerationSupervisor::new(ports);
    let request = GenerationRequest::new(
        Vec::new(),
        backend_ref,
        GenerationStrategy::CausalAutoregressive,
    )
    .with_budget(ResourceBudget {
        max_output_tokens: Some(16),
        timeout: Some(Duration::from_secs(30)),
    });
    let mut rx = supervisor.run(backend, request, None).await.unwrap();
    let mut events = drain_until_started(&mut rx).await;
    sleeper.release();
    hold.notify_one();
    events.extend(collect(rx).await);
    match terminals(&events).as_slice() {
        [TerminalOutcome::TimedOut { identity }] => {
            assert_eq!(identity.actual.model, "slow");
        }
        other => panic!("expected TimedOut, got {other:?} from {events:?}"),
    }
    assert!(
        events.iter().all(|event| !matches!(
            event,
            GenerationEvent::TextDelta { text, .. } if text == "too late"
        )),
        "timed-out attempt must drop late text after it is offered; events={events:?}"
    );
}

#[tokio::test]
async fn test_disconnect_is_terminal_and_retains_identity() {
    let backend_ref = test_ref("cloud", "wire");
    let backend = Arc::new(ScriptedBackend::ready(
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
        vec![ScriptedStep::Event(GenerationEvent::Terminal(
            TerminalOutcome::Disconnected {
                reason: "peer closed".into(),
                identity: finch_generation::GenerationIdentity::pinned(backend_ref.clone()),
            },
        ))],
    ));
    let supervisor = GenerationSupervisor::new(GenerationPorts::test());
    let request = GenerationRequest::new(
        Vec::new(),
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
    );
    let events = collect(supervisor.run(backend, request, None).await.unwrap()).await;
    match terminals(&events).as_slice() {
        [TerminalOutcome::Disconnected { reason, identity }] => {
            assert_eq!(reason, "peer closed");
            assert_eq!(identity.actual, backend_ref);
        }
        other => panic!("expected Disconnected, got {other:?} from {events:?}"),
    }
}

#[tokio::test]
async fn test_switch_drops_stale_backend_completion() {
    let first_ref = test_ref("cloud", "first");
    let second_ref = test_ref("cloud", "second");
    let hold = Arc::new(Notify::new());
    let first = Arc::new(ScriptedBackend::ready(
        first_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
        vec![
            ScriptedStep::Wait(Arc::clone(&hold)),
            text_delta(&first_ref, "from-first", 1),
        ],
    ));
    let second = Arc::new(ScriptedBackend::ready(
        second_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
        vec![text_delta(&second_ref, "from-second", 1)],
    ));
    let supervisor = GenerationSupervisor::new(GenerationPorts::test());
    let first_rx = supervisor
        .run(
            first,
            GenerationRequest::new(
                Vec::new(),
                first_ref.clone(),
                GenerationStrategy::CausalAutoregressive,
            ),
            None,
        )
        .await
        .unwrap();
    let route = finch_generation::RouteDecision {
        selected: second_ref.clone(),
        reason: "caller switched provider".into(),
        rejected: vec![finch_generation::RejectedBackend {
            backend: first_ref.clone(),
            reason: "superseded".into(),
        }],
    };
    let second_rx = supervisor
        .switch(
            second,
            GenerationRequest::new(
                Vec::new(),
                second_ref.clone(),
                GenerationStrategy::CausalAutoregressive,
            ),
            route,
        )
        .await
        .unwrap();
    hold.notify_one();
    let first_events = collect(first_rx).await;
    let second_events = collect(second_rx).await;
    assert!(
        first_events.iter().all(|event| !matches!(
            event,
            GenerationEvent::TextDelta { text, .. } if text == "from-first"
        )),
        "stale backend must not deliver after switch; first={first_events:?} second={second_events:?}"
    );
    match terminals(&second_events).as_slice() {
        [TerminalOutcome::Completed { text, metadata, .. }] => {
            assert_eq!(text, "from-second");
            assert_eq!(
                metadata.identity.actual, second_ref,
                "completed text attributed to the wrong backend: {second_events:?}"
            );
            assert_ne!(metadata.identity.actual, first_ref);
        }
        other => panic!("expected second backend Completed, got {other:?} from {second_events:?}"),
    }
}

#[tokio::test]
async fn test_explicit_fallback_records_rejection_and_keeps_identity() {
    let requested = test_ref("local", "missing");
    let ready = test_ref("cloud", "cloud-primary");
    let loading = Arc::new(ScriptedBackend::with_readiness(
        requested.clone(),
        GenerationStrategy::CausalAutoregressive,
        ReadinessReport::loading(
            LoadPhase::Downloading,
            Duration::from_millis(5),
            ResourceMetadata {
                elapsed_ms: 5,
                peak_memory_bytes: None,
                accelerator: Some("cpu".into()),
            },
        ),
        vec![],
    ));
    let cloud = Arc::new(ScriptedBackend::ready(
        ready.clone(),
        GenerationStrategy::CausalAutoregressive,
        vec![text_delta(&ready, "cloud-primary", 1)],
    ));
    let request = GenerationRequest::new(
        Vec::new(),
        requested.clone(),
        GenerationStrategy::CausalAutoregressive,
    )
    .with_fallback();
    let (selected, decision) =
        select_backend(&request, &[loading, cloud.clone()]).expect("fallback");
    assert_eq!(selected.identity(), ready);
    assert!(
        decision.reason.contains("explicit fallback"),
        "fallback reason missing: {decision:?}"
    );
    assert_eq!(decision.rejected.len(), 1);
    assert_eq!(decision.rejected[0].backend, requested);
    assert!(
        decision.rejected[0].reason.contains("loading"),
        "rejection must name readiness: {decision:?}"
    );

    let supervisor = GenerationSupervisor::new(GenerationPorts::test());
    let events = collect(
        supervisor
            .run(selected, request, Some(decision.clone()))
            .await
            .unwrap(),
    )
    .await;
    let routed = events.iter().find_map(|event| match event {
        GenerationEvent::Route(route) => Some(route),
        _ => None,
    });
    let routed = routed.expect(&format!("missing Route event: {events:?}"));
    assert_eq!(routed.selected, ready);
    match terminals(&events).as_slice() {
        [TerminalOutcome::Completed { metadata, .. }] => {
            assert_eq!(metadata.identity.requested, requested);
            assert_eq!(metadata.identity.resolved, ready);
            assert_eq!(metadata.identity.actual, ready);
        }
        other => panic!("expected Completed with fallback identity, got {other:?} from {events:?}"),
    }
}

#[test]
fn test_select_backend_does_not_fallback_without_explicit_policy() {
    let requested = test_ref("local", "missing");
    let ready = test_ref("cloud", "cloud-primary");
    let loading = Arc::new(ScriptedBackend::with_readiness(
        requested.clone(),
        GenerationStrategy::CausalAutoregressive,
        ReadinessReport::not_loaded(),
        vec![],
    ));
    let cloud = Arc::new(ScriptedBackend::ready(
        ready,
        GenerationStrategy::CausalAutoregressive,
        vec![],
    ));
    let request = GenerationRequest::new(
        Vec::new(),
        requested,
        GenerationStrategy::CausalAutoregressive,
    );
    let error = select_backend(&request, &[loading, cloud])
        .err()
        .expect("implicit fallback must fail")
        .to_string();
    assert!(
        error.contains("no ready generation backend"),
        "implicit fallback leaked a backend: {error}"
    );
}

#[tokio::test]
async fn test_ar_and_refinement_compare_at_matched_budget() {
    let budget = ResourceBudget {
        max_output_tokens: Some(32),
        timeout: None,
    };
    let ar_ref = test_ref("local", "ar");
    let ref_ref = test_ref("local", "refine");
    let ar = Arc::new(ScriptedBackend::ready(
        ar_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
        vec![text_delta(&ar_ref, "ar", 1)],
    ));
    let refine = Arc::new(ScriptedBackend::ready(
        ref_ref.clone(),
        GenerationStrategy::MaskedRefinement,
        vec![text_delta(&ref_ref, "refine", 1)],
    ));
    assert!(budget.matches(&budget));
    assert_eq!(ar.strategy(), GenerationStrategy::CausalAutoregressive);
    assert_eq!(refine.strategy(), GenerationStrategy::MaskedRefinement);
    let supervisor = GenerationSupervisor::new(GenerationPorts::test());
    let ar_events = collect(
        supervisor
            .run(
                ar,
                GenerationRequest::new(
                    Vec::new(),
                    ar_ref.clone(),
                    GenerationStrategy::CausalAutoregressive,
                )
                .with_budget(budget.clone()),
                None,
            )
            .await
            .unwrap(),
    )
    .await;
    let refine_events = collect(
        supervisor
            .run(
                refine,
                GenerationRequest::new(
                    Vec::new(),
                    ref_ref.clone(),
                    GenerationStrategy::MaskedRefinement,
                )
                .with_budget(budget),
                None,
            )
            .await
            .unwrap(),
    )
    .await;
    match (
        terminals(&ar_events).as_slice(),
        terminals(&refine_events).as_slice(),
    ) {
        (
            [TerminalOutcome::Completed {
                text: ar_text,
                metadata: ar_meta,
                ..
            }],
            [TerminalOutcome::Completed {
                text: ref_text,
                metadata: ref_meta,
                ..
            }],
        ) => {
            assert_eq!(ar_text, "ar");
            assert_eq!(ref_text, "refine");
            assert_eq!(ar_meta.identity.actual, ar_ref);
            assert_eq!(ref_meta.identity.actual, ref_ref);
        }
        other => panic!("matched-budget backends must both complete: {other:?}"),
    }
}

#[test]
fn test_provider_tool_use_block_translates_to_tool_call() {
    let identity = finch_generation::GenerationIdentity::pinned(test_ref("claude", "sonnet"));
    let event = translate_provider_chunk(
        StreamChunk::ContentBlockComplete(ContentBlock::ToolUse {
            id: "toolu_9".into(),
            name: "grep".into(),
            input: json!({"pattern": "fn "}),
        }),
        &identity,
        4,
    )
    .expect("tool-use block");
    match event {
        GenerationEvent::ToolCallComplete(ToolCall {
            id,
            name,
            input,
            provenance,
        }) => {
            assert_eq!(id, "toolu_9");
            assert_eq!(name, "grep");
            assert_eq!(input["pattern"], "fn ");
            assert_eq!(provenance.provider, "claude");
            assert_eq!(provenance.model, "sonnet");
        }
        other => panic!("expected ToolCallComplete, got {other:?}"),
    }
}

#[tokio::test]
async fn test_failed_load_cause_is_secret_free_and_terminal() {
    let backend_ref = test_ref("local", "broken");
    let backend = Arc::new(ScriptedBackend::with_readiness(
        backend_ref.clone(),
        GenerationStrategy::CausalAutoregressive,
        ReadinessReport::failed(
            "checksum mismatch",
            Duration::from_millis(9),
            ResourceMetadata {
                elapsed_ms: 9,
                peak_memory_bytes: None,
                accelerator: Some("cpu".into()),
            },
        ),
        vec![],
    ));
    let supervisor = GenerationSupervisor::new(GenerationPorts::test());
    let events = collect(
        supervisor
            .run(
                backend,
                GenerationRequest::new(
                    Vec::new(),
                    backend_ref,
                    GenerationStrategy::CausalAutoregressive,
                ),
                None,
            )
            .await
            .unwrap(),
    )
    .await;
    match terminals(&events).as_slice() {
        [TerminalOutcome::Failed { cause, .. }] => {
            assert_eq!(cause, "checksum mismatch");
            assert!(!cause.to_lowercase().contains("token"));
            assert!(!cause.to_lowercase().contains("secret"));
        }
        other => panic!("expected Failed, got {other:?} from {events:?}"),
    }
}

#[test]
fn test_allowance_is_not_billing() {
    let allowance = Allowance {
        primary_used_percent: Some(12.0),
        secondary_used_percent: None,
    };
    assert_eq!(allowance.primary_used_percent, Some(12.0));
}

struct RecordingProvider {
    default_model: &'static str,
    seen_models: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ProviderBackend for RecordingProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> anyhow::Result<ProviderResponse> {
        let (request, _bindings) = request.into_request_for(self)?;
        anyhow::bail!(
            "non-stream path unused in this test; model={}",
            request.model
        )
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>> {
        let (request, _bindings) = request.into_request_for(self)?;
        self.seen_models
            .lock()
            .expect("seen_models lock")
            .push(request.model.clone());
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let model = request.model;
        tokio::spawn(async move {
            let _ = tx.send(Ok(StreamChunk::TextDelta("ok".into()))).await;
            let _ = tx
                .send(Ok(StreamChunk::ResponseMetadata {
                    model: format!("{model}-served"),
                }))
                .await;
        });
        Ok(rx)
    }

    fn name(&self) -> &str {
        "cloud"
    }

    fn default_model(&self) -> &str {
        self.default_model
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        ModelCapabilities::static_metadata(
            self.name(),
            model,
            "2026-09-16",
            "generation identity fixture",
            CapabilitySupport::Supported,
            CapabilitySupport::Unsupported,
            CapabilitySupport::Unsupported,
            ReasoningCapability::unsupported("2026-09-16", "generation identity fixture"),
            Some(8_000),
            Some(16_000),
            None,
        )
    }
}

#[tokio::test]
async fn test_requested_model_is_not_reported_as_provider_default() {
    let provider = Arc::new(RecordingProvider {
        default_model: "default-model",
        seen_models: std::sync::Mutex::new(Vec::new()),
    });
    let backend = Arc::new(
        ProviderGenerationBackend::new(provider.clone()).expect("wrap recording provider"),
    );
    assert_eq!(
        backend.identity().model,
        "default-model",
        "catalog identity may name the default; dispatch must not: {:?}",
        backend.identity()
    );

    let requested = BackendRef::new("cloud", "requested-model", BackendKind::Cloud).unwrap();
    let supervisor = GenerationSupervisor::new(GenerationPorts::test());
    let events = collect(
        supervisor
            .run(
                backend,
                GenerationRequest::new(
                    vec![finch_providers::Message::user("hi")],
                    requested.clone(),
                    GenerationStrategy::CausalAutoregressive,
                ),
                None,
            )
            .await
            .unwrap(),
    )
    .await;

    let seen = provider
        .seen_models
        .lock()
        .expect("seen_models lock")
        .clone();
    assert_eq!(
        seen,
        vec!["requested-model".to_string()],
        "wire dispatch must send the requested model, not the default; events={events:?}"
    );

    let named_default = events
        .iter()
        .filter(|event| event_names_model(event, "default-model"))
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        named_default.is_empty(),
        "requested run must not be attributed to default-model; leaked={named_default:?} events={events:?}"
    );

    match terminals(&events).as_slice() {
        [TerminalOutcome::Completed { metadata, .. }] => {
            assert_eq!(metadata.identity.requested, requested);
            assert_eq!(
                metadata.identity.resolved.model, "requested-model",
                "resolved must follow the request, not default-model: {metadata:?}"
            );
            assert_eq!(
                metadata.identity.actual.model, "requested-model-served",
                "actual must follow provider-reported serving model: {metadata:?}"
            );
            assert_ne!(metadata.identity.resolved.model, "default-model");
            assert_ne!(metadata.identity.actual.model, "default-model");
        }
        other => {
            panic!("expected Completed with requested identity, got {other:?} from {events:?}")
        }
    }
}

fn event_names_model(event: &GenerationEvent, model: &str) -> bool {
    match event {
        GenerationEvent::Started { identity, .. }
        | GenerationEvent::Identity(identity)
        | GenerationEvent::Terminal(TerminalOutcome::Completed {
            metadata: finch_generation::GenerationMetadata { identity, .. },
            ..
        })
        | GenerationEvent::Terminal(TerminalOutcome::Cancelled { identity })
        | GenerationEvent::Terminal(TerminalOutcome::TimedOut { identity })
        | GenerationEvent::Terminal(TerminalOutcome::Disconnected { identity, .. })
        | GenerationEvent::Terminal(TerminalOutcome::Failed { identity, .. }) => {
            identity.requested.model == model
                || identity.resolved.model == model
                || identity.actual.model == model
        }
        GenerationEvent::TextDelta { provenance, .. }
        | GenerationEvent::ThinkingDelta { provenance, .. }
        | GenerationEvent::ToolCallDelta { provenance, .. } => provenance.model == model,
        GenerationEvent::ToolCallComplete(call) => call.provenance.model == model,
        GenerationEvent::Readiness(_)
        | GenerationEvent::Loading { .. }
        | GenerationEvent::Usage(_)
        | GenerationEvent::Allowance(_)
        | GenerationEvent::Route(_) => false,
    }
}
