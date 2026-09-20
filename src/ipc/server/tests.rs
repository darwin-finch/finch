use super::{
    decode_runner_program_result, decode_runner_turn_result, execute_typed_forth_ipc,
    require_approval_connection, BrainRpcService, BrainRunnerControlImpl, FinchDaemonImpl,
};
use crate::ipc::codec::encode_approval_audience;

#[test]
fn capnp_effect_audit_requires_durable_begin_before_terminal_outcome() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async {
        let temp = tempfile::tempdir().unwrap();
        let store =
            crate::brain::BrainStore::with_root("box.local", Some(temp.path().join("brains")));
        let attachment = store
            .attach(
                "shared",
                "alice",
                crate::brain::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                crate::brain::BrainEventKind::Prompt {
                    text: "effect".into(),
                    attached_mentions: Vec::new(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                crate::brain::BrainRunKind::Interactive,
                prompt.seq,
                attachment.attachment_id,
                crate::brain::BrainRunStatus::Running,
            )
            .unwrap();
        let lease = store
            .acquire_runner_lease("shared", "runner", 1, None, 300_000)
            .unwrap();
        let grant = store
            .issue_effect_audit_authority("shared", run.run_id, lease.lease_id, None)
            .unwrap();
        let original_grant = grant.clone();
        let server = std::sync::Arc::new(
            crate::server::AgentServer::for_brain_protocol_test(
                store.clone(),
                crate::brain::BrainCredentialAuthority::ephemeral([44; 32]),
                "test-password".into(),
                temp.path(),
            )
            .unwrap(),
        );
        let authority = super::BrainEffectAuditRpcAuthority {
            store: store.clone(),
            grant,
            runners: server.brain_runners().clone(),
            brain: "shared".into(),
            lease_id: lease.lease_id,
            connection_id: None,
            active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        let control: super::finch_ipc_capnp::brain_program_control::Client =
            capnp_rpc::new_client(super::BrainProgramControlImpl {
                lifecycle: crate::server::BrainLifecycleService::from_server(&server),
                brain: "shared".into(),
                run_id: run.run_id,
                request_seq: prompt.seq,
                maximum_grant_ceiling: None,
                effect_audit: Some(authority),
            });
        let execution_id = uuid::Uuid::new_v4();
        let mut reserve = control.reserve_effect_request();
        reserve.get().set_execution_id(&execution_id.to_string());
        let effect = crate::vm::VmSideEffect {
            protocol_version: 1,
            sequence: 0,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            output: Vec::new(),
            event: crate::vm::HostSideEffect::Emit {
                text: "hello".into(),
            },
            origin: crate::vm::SourceOrigin::generated("capnp-effect-audit-test"),
        };
        crate::ipc::codec::encode_vm_side_effect(reserve.get().init_effect(), &effect).unwrap();
        let reservation = reserve
            .send()
            .promise
            .await
            .unwrap()
            .get()
            .unwrap()
            .get_reservation()
            .unwrap();
        let permit = reservation
            .begin_request()
            .send()
            .promise
            .await
            .unwrap()
            .get()
            .unwrap()
            .get_permit()
            .unwrap();
        let repeated_begin = reservation
            .begin_request()
            .send()
            .promise
            .await
            .err()
            .expect("a raw capability replay must not mint a second permit");
        assert!(repeated_begin.to_string().contains("already begun"));
        assert!(matches!(
            store.snapshot("shared").unwrap().effect_audits[0].state,
            crate::runtime::EffectAuditState::AwaitingHostResult
        ));
        let mut invalid_terminal = reservation.not_applied_request();
        invalid_terminal.get().set_reason("too late");
        let invalid_terminal_error = invalid_terminal
            .send()
            .promise
            .await
            .err()
            .expect("begun reservation must reject a permit-free outcome");
        assert!(invalid_terminal_error.to_string().contains("host permit"));

        store
            .release_runner_lease("shared", lease.lease_id)
            .unwrap();
        store
            .acquire_runner_lease("shared", "successor", 1, None, 300_000)
            .unwrap();
        let mut stale_reserve = control.reserve_effect_request();
        stale_reserve
            .get()
            .set_execution_id(&uuid::Uuid::new_v4().to_string());
        crate::ipc::codec::encode_vm_side_effect(
            stale_reserve.get().init_effect(),
            &crate::vm::VmSideEffect {
                sequence: 1,
                ..effect.clone()
            },
        )
        .unwrap();
        let stale_reserve_error = stale_reserve
            .send()
            .promise
            .await
            .err()
            .expect("successor lease must invalidate the original reserve capability");
        assert!(stale_reserve_error.to_string().contains("successor"));
        store
            .transition_run(
                "shared",
                "daemon",
                run.run_id,
                crate::brain::BrainRunStatus::Cancelled,
                Some("turn cancelled before host completion".into()),
            )
            .unwrap();
        let mut finish = permit.finish_request();
        finish.get().init_outcome().init_acknowledged(0);
        finish.send().promise.await.unwrap();
        let mut exact_retry = permit.finish_request();
        exact_retry.get().init_outcome().init_acknowledged(0);
        exact_retry.send().promise.await.unwrap();
        let mut conflicting_retry = permit.finish_request();
        conflicting_retry
            .get()
            .init_outcome()
            .set_failed_partial("changed");
        let conflicting_error = conflicting_retry
            .send()
            .promise
            .await
            .err()
            .expect("a raw permit replay cannot change its terminal outcome");
        assert!(conflicting_error.to_string().contains("different outcome"));

        let snapshot = store.snapshot("shared").unwrap();
        assert!(matches!(snapshot.effect_audits[0].state,
                crate::runtime::EffectAuditState::Terminal {
                    outcome: crate::runtime::EffectAuditTerminalOutcome::Redacted {
                        ref outcome_kind
                    }
                } if outcome_kind == "acknowledged"));
        assert!(!snapshot
            .events
            .iter()
            .any(|event| matches!(event.kind, crate::brain::BrainEventKind::ToolResult { .. })));

        // Reconstruct the raw server capability after a daemon/store
        // reload. The original callback and lease are stale and the run
        // is terminal, but a caller that lost the original reserve ACK
        // must still learn that its exact identity is durably fenced.
        let restarted =
            crate::brain::BrainStore::with_root("box.local", Some(temp.path().join("brains")));
        let restarted_server = std::sync::Arc::new(
            crate::server::AgentServer::for_brain_protocol_test(
                restarted.clone(),
                crate::brain::BrainCredentialAuthority::ephemeral([45; 32]),
                "test-password".into(),
                temp.path(),
            )
            .unwrap(),
        );
        let replay_control: super::finch_ipc_capnp::brain_program_control::Client =
            capnp_rpc::new_client(super::BrainProgramControlImpl {
                lifecycle: crate::server::BrainLifecycleService::from_server(&restarted_server),
                brain: "shared".into(),
                run_id: run.run_id,
                request_seq: prompt.seq,
                maximum_grant_ceiling: None,
                effect_audit: Some(super::BrainEffectAuditRpcAuthority {
                    store: restarted.clone(),
                    grant: original_grant,
                    runners: restarted_server.brain_runners().clone(),
                    brain: "shared".into(),
                    lease_id: lease.lease_id,
                    connection_id: None,
                    active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                }),
            });
        let mut replay = replay_control.reserve_effect_request();
        replay.get().set_execution_id(&execution_id.to_string());
        crate::ipc::codec::encode_vm_side_effect(replay.get().init_effect(), &effect).unwrap();
        replay
            .send()
            .promise
            .await
            .unwrap()
            .get()
            .unwrap()
            .get_reservation()
            .unwrap();

        let mut conflicting = replay_control.reserve_effect_request();
        conflicting
            .get()
            .set_execution_id(&execution_id.to_string());
        crate::ipc::codec::encode_vm_side_effect(
            conflicting.get().init_effect(),
            &crate::vm::VmSideEffect {
                event: crate::vm::HostSideEffect::Emit {
                    text: "changed".into(),
                },
                ..effect
            },
        )
        .unwrap();
        let error = conflicting
            .send()
            .promise
            .await
            .err()
            .expect("conflicting raw reserve replay must fail closed after reload");
        assert!(error.to_string().contains("conflicting"));
    }));
}

struct EffectEofRunner {
    begin: bool,
    count: usize,
}

fn delayed_finish_effect_audit(
    control: crate::server::RunnerEffectAuditControl,
    finish_started: tokio::sync::oneshot::Sender<()>,
    finish_release: std::sync::Arc<tokio::sync::Notify>,
    physical_effects: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> crate::server::RunnerEffectAuditControl {
    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::task::spawn_local(async move {
        let Some(crate::server::RunnerEffectAuditControlRequest::Reserve {
            execution_id,
            effect,
            response_tx,
        }) = control_rx.recv().await
        else {
            return;
        };
        let reservation = match control.reserve(execution_id, effect).await {
            Ok(reservation) => reservation,
            Err(error) => {
                let _ = response_tx.send(Err(error));
                return;
            }
        };
        let (reservation_tx, mut reservation_rx) = tokio::sync::mpsc::unbounded_channel();
        let _ = response_tx.send(Ok(crate::server::RunnerEffectAuditReservation::new(
            reservation_tx,
        )));
        let Some(request) = reservation_rx.recv().await else {
            return;
        };
        match request {
            crate::server::RunnerEffectAuditReservationRequest::Begin { response_tx } => {
                let permit = match reservation.begin().await {
                    Ok(permit) => permit,
                    Err(error) => {
                        let _ = response_tx.send(Err(error));
                        return;
                    }
                };
                let (permit_tx, mut permit_rx) = tokio::sync::mpsc::unbounded_channel();
                let _ = response_tx.send(Ok(crate::server::RunnerHostEffectPermit::new(permit_tx)));
                let Some(request) = permit_rx.recv().await else {
                    return;
                };
                physical_effects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = finish_started.send(());
                finish_release.notified().await;
                let result = permit.finish(request.outcome).await;
                let _ = request.response_tx.send(result);
            }
            crate::server::RunnerEffectAuditReservationRequest::NotApplied {
                reason,
                response_tx,
            } => {
                let result = reservation.not_applied(reason).await;
                let _ = response_tx.send(result);
            }
        }
    });
    crate::server::RunnerEffectAuditControl::new(control_tx)
}

struct ProviderSubmitProgramGenerator {
    input: serde_json::Value,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::generators::Generator for ProviderSubmitProgramGenerator {
    async fn generate(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<crate::generators::GeneratorResponse> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        anyhow::ensure!(call == 0, "provider continuation escaped cancellation");
        Ok(crate::generators::GeneratorResponse {
            text: String::new(),
            content_blocks: vec![crate::providers::ContentBlock::ToolUse {
                id: "effect-tool".into(),
                name: "submit_program".into(),
                input: self.input.clone(),
            }],
            tool_uses: vec![crate::generators::ToolUse {
                id: "effect-tool".into(),
                name: "submit_program".into(),
                input: self.input.clone(),
            }],
            metadata: crate::generators::ResponseMetadata {
                generator: "effect-test".into(),
                model: "effect-test".into(),
                confidence: None,
                stop_reason: Some("tool_use".into()),
                input_tokens: None,
                output_tokens: None,
                latency_ms: None,
                primary_allowance_used_percent: None,
                secondary_allowance_used_percent: None,
            },
        })
    }

    async fn generate_stream(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<
        Option<tokio::sync::mpsc::Receiver<anyhow::Result<crate::generators::StreamChunk>>>,
    > {
        Ok(None)
    }

    fn capabilities(&self) -> &crate::generators::GeneratorCapabilities {
        static CAPABILITIES: crate::generators::GeneratorCapabilities =
            crate::generators::GeneratorCapabilities {
                supports_streaming: false,
                supports_tools: true,
                supports_conversation: true,
                max_context_messages: Some(8),
            };
        &CAPABILITIES
    }

    fn name(&self) -> &str {
        "effect-test"
    }
}

impl super::finch_ipc_capnp::brain_runner::Server for EffectEofRunner {
    fn run_program(
        self: capnp::capability::Rc<Self>,
        params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
        _results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        let request = match params.get().and_then(|params| params.get_request()) {
            Ok(request) => request,
            Err(error) => return capnp::capability::Promise::err(error),
        };
        let control = match request.get_control() {
            Ok(control) => control,
            Err(error) => return capnp::capability::Promise::err(error),
        };
        let begin = self.begin;
        let count = self.count;
        capnp::capability::Promise::from_future(async move {
            for sequence in 0..count {
                let mut reserve = control.reserve_effect_request();
                reserve
                    .get()
                    .set_execution_id(&uuid::Uuid::new_v4().to_string());
                crate::ipc::codec::encode_vm_side_effect(
                    reserve.get().init_effect(),
                    &crate::vm::VmSideEffect {
                        protocol_version: 1,
                        sequence: sequence as u64,
                        requirement: crate::vm::CapabilityRequirement {
                            capability: crate::vm::CapabilityKind::SessionEmit,
                            selector: crate::vm::ResourceSelector::None,
                        },
                        output: Vec::new(),
                        event: crate::vm::HostSideEffect::Emit { text: "eof".into() },
                        origin: crate::vm::SourceOrigin::generated("raw-eof-effect-audit"),
                    },
                )
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
                let reservation = reserve.send().promise.await?.get()?.get_reservation()?;
                if begin {
                    let _permit = reservation
                        .begin_request()
                        .send()
                        .promise
                        .await?
                        .get()?
                        .get_permit()?;
                }
            }
            Err(capnp::Error::disconnected(
                "synthetic raw frontend EOF".into(),
            ))
        })
    }

    fn run_turn(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
        _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
    }

    fn cancel_run(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
        _results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
    }

    fn project_memory(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
        _results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
    }
}

struct EffectNormalRunner {
    begin: bool,
    remote_disconnect_error: bool,
    permit_tx: std::cell::RefCell<
        Option<
            tokio::sync::oneshot::Sender<
                Option<super::finch_ipc_capnp::brain_host_effect_permit::Client>,
            >,
        >,
    >,
}

impl super::finch_ipc_capnp::brain_runner::Server for EffectNormalRunner {
    fn run_program(
        self: capnp::capability::Rc<Self>,
        params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
        mut results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        let control = match params
            .get()
            .and_then(|params| params.get_request())
            .and_then(|request| request.get_control())
        {
            Ok(control) => control,
            Err(error) => return capnp::capability::Promise::err(error),
        };
        let begin = self.begin;
        let remote_disconnect_error = self.remote_disconnect_error;
        let permit_tx = self
            .permit_tx
            .borrow_mut()
            .take()
            .expect("normal runner called twice");
        capnp::capability::Promise::from_future(async move {
            let mut reserve = control.reserve_effect_request();
            reserve
                .get()
                .set_execution_id(&uuid::Uuid::new_v4().to_string());
            crate::ipc::codec::encode_vm_side_effect(
                reserve.get().init_effect(),
                &crate::vm::VmSideEffect {
                    protocol_version: 1,
                    sequence: 0,
                    requirement: crate::vm::CapabilityRequirement {
                        capability: crate::vm::CapabilityKind::SessionEmit,
                        selector: crate::vm::ResourceSelector::None,
                    },
                    output: Vec::new(),
                    event: crate::vm::HostSideEffect::Emit {
                        text: "normal".into(),
                    },
                    origin: crate::vm::SourceOrigin::generated("raw-normal-effect-audit"),
                },
            )
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
            let reservation = reserve.send().promise.await?.get()?.get_reservation()?;
            let permit = if begin {
                Some(
                    reservation
                        .begin_request()
                        .send()
                        .promise
                        .await?
                        .get()?
                        .get_permit()?,
                )
            } else {
                None
            };
            let _ = permit_tx.send(permit);
            if remote_disconnect_error {
                return Err(capnp::Error::disconnected(
                    "application exception with a misleading disconnected kind".into(),
                ));
            }
            results
                .get()
                .init_result()
                .set_error("synthetic normal application return");
            Ok(())
        })
    }

    fn run_turn(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
        _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
    }

    fn cancel_run(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
        _results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
    }

    fn project_memory(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
        _results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
    }
}

/// A runner that is reachable but whose connection breaks mid-call, which
/// is what `Promise::err` from the capability models.
struct BrokenConnectionRunner;

impl super::finch_ipc_capnp::brain_runner::Server for BrokenConnectionRunner {
    fn run_turn(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
        _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::disconnected("connection lost".into()))
    }

    fn run_program(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
        _results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::disconnected("connection lost".into()))
    }

    fn cancel_run(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
        _results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::disconnected("connection lost".into()))
    }

    fn project_memory(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
        _results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::disconnected("connection lost".into()))
    }
}

#[tokio::test]
async fn broken_runner_connection_declares_itself_unavailable_to_memory_replay() {
    // #254. A replay pass walks every completed run in a Brain. A broken
    // connection fails identically for all of them, so it has to be
    // distinguishable from "this one turn was declined" — otherwise the
    // pass pays a full IPC round trip and a log line per completed run,
    // under the Brain's execution lock, with nothing to gain.
    //
    // The `Result<usize, String>` wire has no room for a variant, so the
    // daemon marks a systemic failure with `RUNNER_UNAVAILABLE_PREFIX`.
    // This drives the real forwarding path against a real capability, so
    // dropping the prefix from `forward_runner_request` fails here rather
    // than passing on a literal a test supplied itself.
    let temp = tempfile::tempdir().unwrap();
    let store = crate::brain::BrainStore::with_root("box.local", Some(temp.path().join("brains")));
    store
        .attach(
            "shared",
            "alice",
            crate::brain::AttachmentRole::Driver,
            None,
        )
        .unwrap();
    let server = std::sync::Arc::new(
        crate::server::AgentServer::for_brain_protocol_test(
            store.clone(),
            crate::brain::BrainCredentialAuthority::ephemeral([46; 32]),
            "test-password".into(),
            temp.path(),
        )
        .unwrap(),
    );

    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let request = crate::server::RunnerMemoryProjectionRequest {
        brain_id: crate::brain::BrainId(uuid::Uuid::new_v4()),
        brain: "shared".into(),
        run_id: crate::brain::RunId(uuid::Uuid::new_v4()),
        request_seq: 1,
        prompt: "remember this".into(),
        rendered: "remembered".into(),
        response_tx,
    };
    let runner: super::finch_ipc_capnp::brain_runner::Client =
        capnp_rpc::new_client(BrokenConnectionRunner);
    super::forward_test_runner_request(
        runner,
        std::sync::Arc::clone(&server),
        crate::server::RunnerRequest::ProjectMemory(request),
    )
    .await;

    let error = response_rx
        .await
        .unwrap()
        .expect_err("a broken connection cannot project memory");
    assert!(
        error.starts_with(crate::server::RUNNER_UNAVAILABLE_PREFIX),
        "a transport failure must declare itself systemic so replay aborts \
             instead of retrying every run; got {error:?}"
    );
}

async fn raw_effect_eof_states(
    begin: bool,
    count: usize,
    mature_history: bool,
) -> Vec<crate::runtime::EffectAuditState> {
    let temp = tempfile::tempdir().unwrap();
    let store = crate::brain::BrainStore::with_root("box.local", Some(temp.path().join("brains")));
    let attachment = store
        .attach(
            "shared",
            "alice",
            crate::brain::AttachmentRole::Driver,
            None,
        )
        .unwrap();
    let prompt = store
        .push(
            "shared",
            "alice",
            crate::brain::BrainEventKind::Prompt {
                text: "effect eof".into(),
                attached_mentions: Vec::new(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            crate::brain::BrainRunKind::Interactive,
            prompt.seq,
            attachment.attachment_id,
            crate::brain::BrainRunStatus::Running,
        )
        .unwrap();
    let lease = store
        .acquire_runner_lease("shared", "runner", 1, None, 300_000)
        .unwrap();
    if mature_history {
        store
            .seed_mature_effect_audit_history_for_test("shared", 1_024, 3)
            .unwrap();
    }
    let server = std::sync::Arc::new(
        crate::server::AgentServer::for_brain_protocol_test(
            store.clone(),
            crate::brain::BrainCredentialAuthority::ephemeral([45; 32]),
            "test-password".into(),
            temp.path(),
        )
        .unwrap(),
    );
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let request = crate::server::RunnerProgramRequest {
        brain: "shared".into(),
        run_id: run.run_id,
        request_seq: prompt.seq,
        language: crate::brain::ProgramLanguage::Forth,
        source: "noop".into(),
        interaction: crate::server::RunnerProgramInteraction::Interactive,
        grant_ceiling: None,
        control_tx: None,
        effect_audit: None,
        response_tx,
    };
    let runner: super::finch_ipc_capnp::brain_runner::Client =
        capnp_rpc::new_client(EffectEofRunner { begin, count });
    super::forward_test_runner_request(
        runner,
        std::sync::Arc::clone(&server),
        crate::server::RunnerRequest::Program(request),
    )
    .await;
    assert!(response_rx.await.unwrap().is_err());
    store
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .unwrap();
    store
        .snapshot("shared")
        .unwrap()
        .effect_audits
        .into_iter()
        .map(|entry| entry.state)
        .collect()
}

async fn partial_frame_connection_teardown_fixture(
    fail_audit_batch: bool,
) -> (
    tempfile::TempDir,
    crate::brain::BrainStore,
    std::sync::Arc<crate::server::AgentServer>,
    uuid::Uuid,
    crate::brain::RunnerLeaseId,
    anyhow::Result<()>,
) {
    use tokio::io::AsyncWriteExt;

    let temp = tempfile::tempdir().unwrap();
    let store = crate::brain::BrainStore::with_root("box.local", Some(temp.path().join("brains")));
    let attachment = store
        .attach(
            "shared",
            "alice",
            crate::brain::AttachmentRole::Driver,
            None,
        )
        .unwrap();
    let prompt = store
        .push(
            "shared",
            "alice",
            crate::brain::BrainEventKind::Prompt {
                text: "partial frame".into(),
                attached_mentions: Vec::new(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            crate::brain::BrainRunKind::Interactive,
            prompt.seq,
            attachment.attachment_id,
            crate::brain::BrainRunStatus::Running,
        )
        .unwrap();
    let lease = store
        .acquire_runner_lease("shared", "runner", 1, None, 300_000)
        .unwrap();
    let connection_id = uuid::Uuid::new_v4();
    let grant = store
        .issue_effect_audit_authority(
            "shared",
            run.run_id,
            lease.lease_id,
            Some(crate::brain::ConnectionId(connection_id)),
        )
        .unwrap();
    store
        .reserve_effect_audit(
            &grant,
            uuid::Uuid::new_v4(),
            crate::vm::VmSideEffect {
                protocol_version: 1,
                sequence: 0,
                requirement: crate::vm::CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::SessionEmit,
                    selector: crate::vm::ResourceSelector::None,
                },
                output: Vec::new(),
                event: crate::vm::HostSideEffect::Emit {
                    text: "unbegun".into(),
                },
                origin: crate::vm::SourceOrigin::generated("partial-frame-unbegun"),
            },
        )
        .unwrap();
    let begun = store
        .reserve_effect_audit(
            &grant,
            uuid::Uuid::new_v4(),
            crate::vm::VmSideEffect {
                protocol_version: 1,
                sequence: 1,
                requirement: crate::vm::CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::SessionEmit,
                    selector: crate::vm::ResourceSelector::None,
                },
                output: Vec::new(),
                event: crate::vm::HostSideEffect::Emit {
                    text: "begun".into(),
                },
                origin: crate::vm::SourceOrigin::generated("partial-frame-begun"),
            },
        )
        .unwrap();
    let _permit = store.begin_effect_audit(&grant, begun).unwrap();
    let server = std::sync::Arc::new(
        crate::server::AgentServer::for_brain_protocol_test(
            store.clone(),
            crate::brain::BrainCredentialAuthority::ephemeral([49; 32]),
            "test-password".into(),
            temp.path(),
        )
        .unwrap(),
    );
    let runners = server.brain_runners();
    runners
        .claim_connection_identity(connection_id, "runner@box.local/partial")
        .unwrap();
    runners
        .claim_connection_lease(connection_id, "shared", lease.lease_id)
        .unwrap();
    let (callback_tx, _callback_rx) = tokio::sync::mpsc::unbounded_channel();
    runners
        .register_for_connection(connection_id, "shared", lease.lease_id, callback_tx)
        .unwrap();
    if fail_audit_batch {
        store
            .fail_next_effect_audit_batch_for_test("shared")
            .unwrap();
    }

    let (server_stream, mut peer_stream) = tokio::net::UnixStream::pair().unwrap();
    let handler_server = std::sync::Arc::clone(&server);
    let handler = tokio::task::spawn_local(async move {
        super::handle_connection_with_id(server_stream, handler_server, connection_id).await
    });
    tokio::task::yield_now().await;
    // Declare one eight-byte segment, write only half its payload, then
    // close. This is a real Cap'n Proto partial frame rather than a remote
    // application exception choosing an error kind. RpcSystem is allowed
    // to normalize peer EOF to Ok; bounded lifecycle teardown, not the
    // method-level error value, owns reconciliation.
    peer_stream
        .write_all(&[0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0])
        .await
        .unwrap();
    peer_stream.shutdown().await.unwrap();
    drop(peer_stream);
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), handler)
        .await
        .expect("partial-frame connection teardown exceeded two seconds")
        .unwrap();
    (temp, store, server, connection_id, lease.lease_id, result)
}

#[tokio::test(flavor = "current_thread")]
async fn effect_audit_partial_frame_connection_teardown_reconciles_before_and_after_begin() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (_temp, store, server, _connection_id, lease_id, _result) =
                partial_frame_connection_teardown_fixture(false).await;
            assert!(!server.brain_runners().has_registration("shared", lease_id));
            let states = store
                .snapshot("shared")
                .unwrap()
                .effect_audits
                .into_iter()
                .map(|entry| entry.state)
                .collect::<Vec<_>>();
            assert!(states.iter().any(|state| matches!(
                state,
                crate::runtime::EffectAuditState::Terminal {
                    outcome: crate::runtime::EffectAuditTerminalOutcome::AbandonedNotApplied
                }
            )));
            assert!(states.iter().any(|state| matches!(
                state,
                crate::runtime::EffectAuditState::Terminal {
                    outcome: crate::runtime::EffectAuditTerminalOutcome::UncertainProcessLoss
                }
            )));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn effect_audit_teardown_transaction_failure_keeps_authority_fenced_until_retry() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (_temp, store, server, connection_id, lease_id, result) =
                partial_frame_connection_teardown_fixture(true).await;
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("could not reconcile effect audits"));
            let replacement = uuid::Uuid::new_v4();
            assert!(server
                .brain_runners()
                .claim_connection_identity(replacement, "runner@box.local/partial")
                .is_err());
            assert!(server
                .brain_runners()
                .claim_connection_lease(replacement, "shared", lease_id)
                .is_err());
            assert_eq!(
                store
                    .reconcile_effect_audits_for_disconnected_leases("shared", &[lease_id])
                    .unwrap(),
                2
            );
            server
                .brain_runners()
                .begin_connection_teardown(connection_id)
                .finish()
                .unwrap();
            server
                .brain_runners()
                .claim_connection_identity(replacement, "runner@box.local/partial")
                .unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn effect_audit_connection_teardown_closes_admission_and_drains_pre_snapshot_dispatch() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let store =
                crate::brain::BrainStore::with_root("box.local", Some(temp.path().join("brains")));
            let attachment = store
                .attach(
                    "shared",
                    "alice",
                    crate::brain::AttachmentRole::Driver,
                    None,
                )
                .unwrap();
            let prompt = store
                .push(
                    "shared",
                    "alice",
                    crate::brain::BrainEventKind::Prompt {
                        text: "queued teardown race".into(),
                        attached_mentions: Vec::new(),
                    },
                )
                .unwrap();
            let run = store
                .start_run(
                    "shared",
                    "alice",
                    crate::brain::BrainRunKind::Interactive,
                    prompt.seq,
                    attachment.attachment_id,
                    crate::brain::BrainRunStatus::Running,
                )
                .unwrap();
            let lease = store
                .acquire_runner_lease("shared", "runner", 1, None, 300_000)
                .unwrap();
            let server = std::sync::Arc::new(
                crate::server::AgentServer::for_brain_protocol_test(
                    store.clone(),
                    crate::brain::BrainCredentialAuthority::ephemeral([50; 32]),
                    "test-password".into(),
                    temp.path(),
                )
                .unwrap(),
            );
            let connection_id = uuid::Uuid::new_v4();
            let runners = server.brain_runners();
            runners
                .claim_connection_identity(connection_id, "runner@box.local/queued")
                .unwrap();
            runners
                .claim_connection_lease(connection_id, "shared", lease.lease_id)
                .unwrap();
            let (callback_tx, _callback_rx) = tokio::sync::mpsc::unbounded_channel();
            runners
                .register_for_connection(connection_id, "shared", lease.lease_id, callback_tx)
                .unwrap();
            let admission = runners
                .connection_dispatch_admission(connection_id)
                .unwrap();
            let queued_dispatch = admission
                .try_enter()
                .expect("live connection admits queued callback dispatch");
            let run_id = run.run_id;
            let lease_id = lease.lease_id;
            let release = std::sync::Arc::new(tokio::sync::Notify::new());
            let release_task = std::sync::Arc::clone(&release);
            let queued_store = store.clone();
            let queued = tokio::task::spawn_local(async move {
                let _queued_dispatch = queued_dispatch;
                release_task.notified().await;
                let grant = queued_store
                    .issue_effect_audit_authority(
                        "shared",
                        run_id,
                        lease_id,
                        Some(crate::brain::ConnectionId(connection_id)),
                    )
                    .unwrap();
                queued_store
                    .reserve_effect_audit(
                        &grant,
                        uuid::Uuid::new_v4(),
                        crate::vm::VmSideEffect {
                            protocol_version: 1,
                            sequence: 0,
                            requirement: crate::vm::CapabilityRequirement {
                                capability: crate::vm::CapabilityKind::SessionEmit,
                                selector: crate::vm::ResourceSelector::None,
                            },
                            output: Vec::new(),
                            event: crate::vm::HostSideEffect::Emit {
                                text: "queued".into(),
                            },
                            origin: crate::vm::SourceOrigin::generated("queued-before-teardown"),
                        },
                    )
                    .unwrap()
            });

            let teardown = runners.begin_connection_teardown(connection_id);
            assert!(
                admission.try_enter().is_none(),
                "teardown must reject new callback work before the audit snapshot"
            );
            release.notify_one();
            teardown.wait_quiesced().await;
            let identity = queued.await.unwrap();
            assert_eq!(
                store
                    .reconcile_effect_audits_for_disconnected_leases("shared", &[lease_id],)
                    .unwrap(),
                1
            );
            teardown.finish().unwrap();
            let snapshot = store.snapshot("shared").unwrap();
            assert!(snapshot
                .effect_audits
                .iter()
                .any(|entry| entry.intent.identity == identity
                    && matches!(
                        entry.state,
                        crate::runtime::EffectAuditState::Terminal {
                            outcome:
                                crate::runtime::EffectAuditTerminalOutcome::AbandonedNotApplied
                        }
                    )));
            assert!(snapshot
                .effect_audits
                .iter()
                .all(|entry| entry.state.is_terminal()));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn effect_audit_raw_frontend_eof_reconciles_before_and_after_application_boundary() {
    assert!(matches!(
        raw_effect_eof_states(false, 1, false).await.remove(0),
        crate::runtime::EffectAuditState::Terminal {
            outcome: crate::runtime::EffectAuditTerminalOutcome::AbandonedNotApplied
        }
    ));
    assert!(matches!(
        raw_effect_eof_states(true, 1, false).await.remove(0),
        crate::runtime::EffectAuditState::Terminal {
            outcome: crate::runtime::EffectAuditTerminalOutcome::UncertainProcessLoss
        }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn effect_audit_max_quota_raw_eof_terminalizes_once_within_teardown_bound() {
    let states = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        raw_effect_eof_states(
            false,
            crate::runtime::MAX_ACTIVE_EFFECT_AUDITS_PER_RUN,
            true,
        ),
    )
    .await
    .expect("max-quota runner EOF exceeded the two-second teardown bound");
    assert_eq!(
        states.len(),
        crate::runtime::MAX_ACTIVE_EFFECT_AUDITS_PER_RUN
    );
    assert!(states.into_iter().all(|state| matches!(
        state,
        crate::runtime::EffectAuditState::Terminal {
            outcome: crate::runtime::EffectAuditTerminalOutcome::AbandonedNotApplied
        }
    )));
}

async fn raw_normal_effect_state(
    begin: bool,
    remote_disconnect_error: bool,
) -> (
    tempfile::TempDir,
    crate::brain::BrainStore,
    std::sync::Arc<crate::server::AgentServer>,
    crate::brain::RunnerLeaseId,
    tokio::sync::mpsc::UnboundedReceiver<crate::server::RunnerRequest>,
    crate::runtime::EffectAuditState,
    Option<super::finch_ipc_capnp::brain_host_effect_permit::Client>,
) {
    let temp = tempfile::tempdir().unwrap();
    let store = crate::brain::BrainStore::with_root("box.local", Some(temp.path().join("brains")));
    let attachment = store
        .attach(
            "shared",
            "alice",
            crate::brain::AttachmentRole::Driver,
            None,
        )
        .unwrap();
    let prompt = store
        .push(
            "shared",
            "alice",
            crate::brain::BrainEventKind::Prompt {
                text: "normal effect".into(),
                attached_mentions: Vec::new(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            crate::brain::BrainRunKind::Interactive,
            prompt.seq,
            attachment.attachment_id,
            crate::brain::BrainRunStatus::Running,
        )
        .unwrap();
    let lease = store
        .acquire_runner_lease("shared", "runner", 1, None, 300_000)
        .unwrap();
    let server = std::sync::Arc::new(
        crate::server::AgentServer::for_brain_protocol_test(
            store.clone(),
            crate::brain::BrainCredentialAuthority::ephemeral([46; 32]),
            "test-password".into(),
            temp.path(),
        )
        .unwrap(),
    );
    let connection_id = uuid::Uuid::new_v4();
    server
        .brain_runners()
        .claim_connection_identity(connection_id, "runner@box.local/application-error")
        .unwrap();
    server
        .brain_runners()
        .claim_connection_lease(connection_id, "shared", lease.lease_id)
        .unwrap();
    let (callback_tx, callback_rx) = tokio::sync::mpsc::unbounded_channel();
    server
        .brain_runners()
        .register_for_connection(connection_id, "shared", lease.lease_id, callback_tx)
        .unwrap();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let request = crate::server::RunnerProgramRequest {
        brain: "shared".into(),
        run_id: run.run_id,
        request_seq: prompt.seq,
        language: crate::brain::ProgramLanguage::Forth,
        source: "noop".into(),
        interaction: crate::server::RunnerProgramInteraction::Interactive,
        grant_ceiling: None,
        control_tx: None,
        effect_audit: None,
        response_tx,
    };
    let (permit_tx, permit_rx) = tokio::sync::oneshot::channel();
    let runner: super::finch_ipc_capnp::brain_runner::Client =
        capnp_rpc::new_client(EffectNormalRunner {
            begin,
            remote_disconnect_error,
            permit_tx: std::cell::RefCell::new(Some(permit_tx)),
        });
    super::forward_test_runner_request(
        runner,
        std::sync::Arc::clone(&server),
        crate::server::RunnerRequest::Program(request),
    )
    .await;
    assert!(response_rx.await.unwrap().is_err());
    let permit = permit_rx.await.unwrap();
    let state = store.snapshot("shared").unwrap().effect_audits[0]
        .state
        .clone();
    (
        temp,
        store,
        server,
        lease.lease_id,
        callback_rx,
        state,
        permit,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn effect_audit_normal_return_abandons_only_unbegun_and_allows_late_finish() {
    let (_temp, _store, _server, _lease_id, _callback_rx, state, permit) =
        raw_normal_effect_state(false, false).await;
    assert!(permit.is_none());
    assert!(matches!(
        state,
        crate::runtime::EffectAuditState::Terminal {
            outcome: crate::runtime::EffectAuditTerminalOutcome::AbandonedNotApplied
        }
    ));

    let (_temp, store, _server, _lease_id, _callback_rx, state, permit) =
        raw_normal_effect_state(true, false).await;
    assert!(matches!(
        state,
        crate::runtime::EffectAuditState::AwaitingHostResult
    ));
    let permit = permit.expect("begun normal-return effect retained its detached permit");
    let mut finish = permit.finish_request();
    finish.get().init_outcome().init_acknowledged(0);
    finish.send().promise.await.unwrap();
    assert!(
        matches!(store.snapshot("shared").unwrap().effect_audits[0].state,
            crate::runtime::EffectAuditState::Terminal {
                outcome: crate::runtime::EffectAuditTerminalOutcome::Redacted {
                    ref outcome_kind
                }
            } if outcome_kind == "acknowledged")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn effect_audit_remote_disconnected_exception_does_not_claim_transport_teardown() {
    let (_temp, store, server, lease_id, _callback_rx, state, permit) =
        raw_normal_effect_state(true, true).await;
    assert!(
        server.brain_runners().has_registration("shared", lease_id),
        "a remote method exception must not revoke the live callback registration"
    );
    assert!(matches!(
        state,
        crate::runtime::EffectAuditState::AwaitingHostResult
    ));
    let permit = permit.expect("begun effect retains its detached completion authority");
    let mut finish = permit.finish_request();
    finish.get().init_outcome().init_acknowledged(0);
    finish.send().promise.await.unwrap();
    assert!(
        matches!(store.snapshot("shared").unwrap().effect_audits[0].state,
                crate::runtime::EffectAuditState::Terminal {
                    outcome: crate::runtime::EffectAuditTerminalOutcome::Redacted {
                        ref outcome_kind
                    }
                } if outcome_kind == "acknowledged")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn effect_audit_provider_turn_cancel_disconnect_late_finish_has_no_publication() {
    tokio::task::LocalSet::new()
            .run_until(async {
                use crate::tools::Tool;

                let temp = tempfile::tempdir().unwrap();
                let task_output = temp.path().join("task-output");
                std::fs::create_dir_all(&task_output).unwrap();
                let store = crate::brain::BrainStore::with_root(
                    "box.local",
                    Some(temp.path().join("brains")),
                );
                let server = std::sync::Arc::new(
                    crate::server::AgentServer::for_brain_protocol_test(
                        store.clone(),
                        crate::brain::BrainCredentialAuthority::ephemeral([48; 32]),
                        "test-password".into(),
                        temp.path(),
                    )
                    .unwrap(),
                );
                let daemon: super::finch_ipc_capnp::finch_daemon::Client =
                    capnp_rpc::new_client(FinchDaemonImpl::new(
                        std::sync::Arc::clone(&server),
                        uuid::Uuid::new_v4(),
                    ));
                let ipc = crate::ipc::IpcClient::from_test_client(daemon);
                let initial = ipc.brain_snapshot("shared").await.unwrap();
                let runner_subject = "runner@box.local/frontend-audit";
                ipc.brain_claim_runner_identity(runner_subject).await.unwrap();
                let lease = ipc
                    .brain_acquire_runner(
                        "shared",
                        runner_subject,
                        &initial.environment,
                        None,
                        300_000,
                    )
                    .await
                    .unwrap();

                let runtime = std::sync::Arc::new(crate::runtime::ProgramRuntime::new());
                runtime.bind_task_output_root(&task_output).unwrap();
                let requirement = crate::vm::CapabilityRequirement::file(
                    crate::vm::FileOperation::Write,
                    crate::vm::FileSelector::parse("${task.output}/**").unwrap(),
                );
                runtime
                    .grant_typed_capability(requirement.clone())
                    .unwrap();
                let input = serde_json::json!({
                    "language": "forth",
                    "source": "s\" late.txt\" task-output-path s\" durable\" bytes task-output-file-write",
                    "intent": "production IPC provider effect audit",
                    "declared_capabilities": [requirement],
                    "manifest_generation": runtime.manifest_generation(),
                });
                let provider_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let generator: std::sync::Arc<dyn crate::generators::Generator> =
                    std::sync::Arc::new(ProviderSubmitProgramGenerator {
                        input,
                        calls: std::sync::Arc::clone(&provider_calls),
                    });
                let submit_tool =
                    crate::tools::SubmitProgramTool::new(
                        std::sync::Arc::clone(&runtime),
                    );
                let definitions = vec![submit_tool.definition()];
                let mut registry = crate::tools::ToolRegistry::new();
                registry.register(Box::new(submit_tool));
                let permissions = crate::tools::PermissionManager::new()
                    .with_default_rule(crate::tools::PermissionRule::Allow);
                let executor = std::sync::Arc::new(tokio::sync::Mutex::new(
                    crate::tools::ToolExecutor::new(
                        registry,
                        permissions,
                        temp.path().join("tool-patterns.json"),
                    )
                    .unwrap(),
                ));

                let (finish_started_tx, finish_started_rx) = tokio::sync::oneshot::channel();
                let finish_release = std::sync::Arc::new(tokio::sync::Notify::new());
                let finish_sender = std::sync::Arc::new(std::sync::Mutex::new(Some(
                    finish_started_tx,
                )));
                let physical_effects =
                    std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let mut event_loop =
                    crate::cli::EventLoop::new_named_brain_test_runner(
                        generator,
                        definitions,
                        executor,
                        runtime,
                    );
                let conversation = event_loop.conversation_for_test();
                let wrapper_release = std::sync::Arc::clone(&finish_release);
                let wrapper_effects = std::sync::Arc::clone(&physical_effects);
                event_loop.set_effect_audit_test_wrapper(std::sync::Arc::new(move |control| {
                    let finish_started = finish_sender
                        .lock()
                        .expect("finish sender lock poisoned")
                        .take()
                        .expect("provider turn reserved more than one host effect");
                    delayed_finish_effect_audit(
                        control,
                        finish_started,
                        std::sync::Arc::clone(&wrapper_release),
                        std::sync::Arc::clone(&wrapper_effects),
                    )
                }));
                let (event_tx, event_driver) =
                    event_loop.start_named_brain_test_runner("shared".into());
                let _bootstrap = ipc
                    .register_brain_runner("shared", lease.lease_id, event_tx.clone())
                    .await
                    .unwrap();
                let attachment = ipc
                    .brain_attach(
                        "shared",
                        "alice",
                        crate::brain::AttachmentRole::Driver,
                        None,
                    )
                    .await
                    .unwrap();
                let mut watch = ipc.brain_watch("shared", &attachment).await.unwrap();
                let _initial_watch = watch.recv().await.unwrap().unwrap();
                let submit_ipc = ipc.clone();
                let submit_attachment = attachment.clone();
                let submission = tokio::task::spawn_local(async move {
                    submit_ipc
                        .brain_submit(
                            "shared",
                            &submit_attachment,
                            crate::brain::BrainEventKind::Prompt {
                                text: "apply one provider effect".into(),
                                attached_mentions: Vec::new(),
                            },
                        )
                        .await
                });

                tokio::time::timeout(std::time::Duration::from_secs(2), finish_started_rx)
                    .await
                    .expect("physical effect did not reach its late finish boundary")
                    .unwrap();
                assert_eq!(
                    std::fs::read(task_output.join("late.txt")).unwrap(),
                    b"durable"
                );
                assert_eq!(physical_effects.load(std::sync::atomic::Ordering::SeqCst), 1);
                let run = store
                    .snapshot("shared")
                    .unwrap()
                    .runs
                    .into_iter()
                    .find(|run| run.status == crate::brain::BrainRunStatus::Running)
                    .expect("daemon did not create the active provider run");
                let cancelled = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    ipc.brain_cancel_run("shared", &attachment, run.run_id),
                )
                .await
                .expect("real daemon/IPC cancellation exceeded the teardown bound")
                .unwrap();
                assert_eq!(
                    cancelled.status,
                    crate::brain::BrainRunStatus::Cancelled
                );
                let submission = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    submission,
                )
                .await
                .expect("real runner cancellation did not quiesce the provider turn")
                .unwrap()
                .unwrap();
                assert_eq!(submission.run.unwrap().run_id, run.run_id);
                let conversation_before_late_finish = serde_json::to_value(
                    conversation.read().await.get_messages(),
                )
                .unwrap();
                event_tx.send(crate::cli::ReplEvent::Shutdown).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(2), event_driver)
                    .await
                    .expect("runner EventLoop disconnect exceeded the teardown bound")
                    .unwrap()
                    .unwrap();
                finish_release.notify_one();
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
                let snapshot = loop {
                    let snapshot = store.snapshot("shared").unwrap();
                    if snapshot.effect_audits.first().is_some_and(|audit| {
                        matches!(
                            audit.state,
                            crate::runtime::EffectAuditState::Terminal {
                                outcome:
                                    crate::runtime::EffectAuditTerminalOutcome::Redacted {
                                        ref outcome_kind
                                    }
                            } if outcome_kind == "acknowledged"
                        )
                    }) {
                        break snapshot;
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "late authoritative effect finish was not durably acknowledged; state={:?}",
                        snapshot.effect_audits.first().map(|audit| &audit.state)
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                };
                assert_eq!(snapshot.effect_audits.len(), 1);
                assert_eq!(physical_effects.load(std::sync::atomic::Ordering::SeqCst), 1);
                assert_eq!(provider_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
                assert!(matches!(snapshot.effect_audits[0].state,
                    crate::runtime::EffectAuditState::Terminal {
                        outcome: crate::runtime::EffectAuditTerminalOutcome::Redacted {
                            ref outcome_kind
                        }
                    } if outcome_kind == "acknowledged"));
                let restarted = crate::brain::BrainStore::with_root(
                    "box.local",
                    Some(temp.path().join("brains")),
                );
                let replayed = restarted.snapshot("shared").unwrap();
                assert_eq!(
                    replayed.effect_audits.len(),
                    1,
                    "restart/replay must reconstruct exactly one terminal audit identity"
                );
                assert_eq!(
                    replayed.effect_audits[0].intent.identity,
                    snapshot.effect_audits[0].intent.identity
                );
                assert!(matches!(replayed.effect_audits[0].state,
                    crate::runtime::EffectAuditState::Terminal {
                        outcome: crate::runtime::EffectAuditTerminalOutcome::Compacted {
                            ref outcome_kind, ..
                        }
                    } if outcome_kind == "acknowledged"));
                assert!(!snapshot.events.iter().any(|event| matches!(
                    event.kind,
                    crate::brain::BrainEventKind::ToolResult { .. }
                        | crate::brain::BrainEventKind::Result { .. }
                        | crate::brain::BrainEventKind::Program { .. }
                        | crate::brain::BrainEventKind::RuntimeCommitted { .. }
                        | crate::brain::BrainEventKind::EffectRecorded { .. }
                )));
                assert_eq!(
                    serde_json::to_value(conversation.read().await.get_messages()).unwrap(),
                    conversation_before_late_finish,
                    "late effect completion must not append provider or ToolResult history"
                );
            })
            .await;
}

struct SocketApprovalRunner {
    failed_tx: std::cell::RefCell<Option<tokio::sync::oneshot::Sender<String>>>,
}

impl super::finch_ipc_capnp::brain_runner::Server for SocketApprovalRunner {
    fn run_program(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
        _results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::unimplemented(
            "socket approval runner accepts only turns".into(),
        ))
    }

    fn run_turn(
        self: capnp::capability::Rc<Self>,
        params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
        _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        let request = match params.get().and_then(|params| params.get_request()) {
            Ok(request) => request,
            Err(error) => return capnp::capability::Promise::err(error),
        };
        let request_seq = request.get_request_seq();
        let audience = match request
            .get_approval_audience()
            .map_err(anyhow::Error::new)
            .and_then(super::decode_approval_audience)
        {
            Ok(audience) => audience,
            Err(error) => {
                return capnp::capability::Promise::err(capnp::Error::failed(error.to_string()))
            }
        };
        let control = match request.get_control() {
            Ok(control) => control,
            Err(error) => return capnp::capability::Promise::err(error),
        };
        let failed_tx = self
            .failed_tx
            .borrow_mut()
            .take()
            .expect("runner received more than one turn");
        capnp::capability::Promise::from_future(async move {
            let mut call = control.request_approval_request();
            crate::ipc::client::encode_brain_turn_event(
                call.get().init_event(),
                &crate::server::RunnerTurnEvent::ApprovalRequested {
                    approval_id: "socket-approval".into(),
                    approval_kind: "tool".into(),
                    subject: "bash".into(),
                    audience,
                    detail: serde_json::json!({"input": {"command": "true"}}),
                },
            )?;
            let error = match call.send().promise.await {
                Ok(_) => "approval unexpectedly succeeded".to_string(),
                Err(error) => error.to_string(),
            };
            let _ = failed_tx.send(error.clone());
            Err(capnp::Error::failed(format!(
                "approval for request {request_seq} failed closed: {error}"
            )))
        })
    }

    fn cancel_run(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
        _results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::unimplemented(
            "socket approval runner does not cancel".into(),
        ))
    }

    fn project_memory(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
        _results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::err(capnp::Error::unimplemented(
            "socket approval runner does not project memory".into(),
        ))
    }
}

#[test]
fn unix_socket_disconnect_fails_reverse_approval_for_exact_attachment_generation() {
    const IPC_TEST_BOUND: std::time::Duration = std::time::Duration::from_secs(2);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let supervised_proof = crate::brain::isolated_test_proof_if_present().unwrap();
            let socket_path = supervised_proof
                .as_ref()
                .map(|proof| proof.ipc_socket.clone())
                .unwrap_or_else(|| temp.path().join("finch.sock"));
            let _socket_path = supervised_proof.is_none().then(|| {
                crate::ipc::transport::set_test_sock_path(socket_path.clone())
            });
            let store = crate::brain::BrainStore::with_root(
                "box.local",
                Some(temp.path().join("brains")),
            );
            let server = std::sync::Arc::new(
                crate::server::AgentServer::for_brain_protocol_test(
                    store.clone(),
                    crate::brain::BrainCredentialAuthority::ephemeral([91; 32]),
                    "test-password".into(),
                    temp.path(),
                )
                .unwrap(),
            );
            let shutdown = tokio_util::sync::CancellationToken::new();
            let server_task =
                tokio::task::spawn_local(super::start_ipc_server(server.clone(), shutdown.clone()));
            if supervised_proof.is_none() {
                if tokio::time::timeout(IPC_TEST_BOUND, async {
                    while !socket_path.exists() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .is_err()
                {
                    panic!(
                        "IPC server did not publish socket {} within {IPC_TEST_BOUND:?}; server_task_finished={}",
                        socket_path.display(),
                        server_task.is_finished()
                    );
                }
            }

            let participant = crate::ipc::IpcClient::connect_path(socket_path.clone())
                .await
                .unwrap();
            let attachment = participant
                .brain_attach(
                    "shared",
                    "alice",
                    crate::brain::AttachmentRole::Driver,
                    None,
                )
                .await
                .unwrap();
            let mut participant_events = participant
                .brain_watch("shared", &attachment)
                .await
                .unwrap();
            participant_events.recv().await.unwrap().unwrap();

            let runner = crate::ipc::IpcClient::connect_path(socket_path.clone())
                .await
                .unwrap();
            let snapshot = runner.brain_snapshot("shared").await.unwrap();
            runner
                .brain_claim_runner_identity("runner@box.local/socket")
                .await
                .unwrap();
            let lease = runner
                .brain_acquire_runner(
                    "shared",
                    "runner@box.local/socket",
                    &snapshot.environment,
                    None,
                    60_000,
                )
                .await
                .unwrap();
            let (failed_tx, failed_rx) = tokio::sync::oneshot::channel();
            let callback: super::finch_ipc_capnp::brain_runner::Client =
                capnp_rpc::new_client(SocketApprovalRunner {
                    failed_tx: std::cell::RefCell::new(Some(failed_tx)),
                });
            runner
                .register_test_brain_runner_client("shared", lease.lease_id, callback)
                .await
                .unwrap();

            let run = participant
                .brain_start_speculative("shared", &attachment, "request approval".into())
                .await
                .unwrap();
            if tokio::time::timeout(IPC_TEST_BOUND, async {
                loop {
                    let current = store.snapshot("shared").unwrap();
                    if current.events.iter().any(|event| {
                        matches!(
                            &event.kind,
                            crate::brain::BrainEventKind::ApprovalRequested {
                                approval_id, ..
                            } if approval_id == "socket-approval"
                        )
                    }) {
                        assert_eq!(
                            store.inspect_run("shared", run.run_id).unwrap().status,
                            crate::brain::BrainRunStatus::AwaitingApproval
                        );
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err()
            {
                let snapshot = store.snapshot("shared").unwrap();
                let run_status = store.inspect_run("shared", run.run_id).unwrap().status;
                panic!(
                    "reverse approval did not become durable within {IPC_TEST_BOUND:?}; run_id={:?}; run_status={run_status:?}; event_count={}",
                    run.run_id,
                    snapshot.events.len()
                );
            }

            let old_connection = attachment.connection_id.unwrap();
            drop(participant_events);
            drop(participant);
            let error = match tokio::time::timeout(IPC_TEST_BOUND, failed_rx).await {
                Ok(Ok(error)) => error,
                Ok(Err(error)) => panic!(
                    "reverse approval callback closed without reporting the disconnect; run_id={:?}; channel_error={error}",
                    run.run_id
                ),
                Err(_) => {
                    let run_status = store.inspect_run("shared", run.run_id).unwrap().status;
                    let connection_still_live = store
                        .require_connection(
                            "shared",
                            attachment.attachment_id,
                            old_connection,
                        )
                        .is_ok();
                    panic!(
                        "physical IPC loss did not fail approval within {IPC_TEST_BOUND:?}; run_id={:?}; run_status={run_status:?}; attachment_id={:?}; connection_id={old_connection:?}; connection_still_live={connection_still_live}",
                        run.run_id,
                        attachment.attachment_id
                    );
                }
            };
            assert!(error.contains("approval audience disconnected"), "{error}");
            if tokio::time::timeout(IPC_TEST_BOUND, async {
                while store
                    .require_connection("shared", attachment.attachment_id, old_connection)
                    .is_ok()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err()
            {
                let run_status = store.inspect_run("shared", run.run_id).unwrap().status;
                panic!(
                    "physical IPC loss did not detach the exact generation within {IPC_TEST_BOUND:?}; run_id={:?}; run_status={run_status:?}; attachment_id={:?}; connection_id={old_connection:?}",
                    run.run_id,
                    attachment.attachment_id
                );
            }

            let replacement = crate::ipc::IpcClient::connect_path(socket_path)
                .await
                .unwrap();
            let replacement_attachment = replacement
                .brain_attach(
                    "shared",
                    "alice",
                    crate::brain::AttachmentRole::Driver,
                    Some(attachment.attachment_id),
                )
                .await
                .unwrap();
            let replacement_connection = replacement_attachment.connection_id.unwrap();
            let mut replacement_events = replacement
                .brain_watch("shared", &replacement_attachment)
                .await
                .unwrap();
            replacement_events.recv().await.unwrap().unwrap();
            assert!(store
                .require_connection(
                    "shared",
                    replacement_attachment.attachment_id,
                    replacement_connection,
                )
                .is_ok());
            assert_eq!(store.snapshot("shared").unwrap().runner_lease, Some(lease));
            let terminal = store.snapshot("shared").unwrap();
            assert_eq!(
                terminal
                    .events
                    .iter()
                    .filter(|event| matches!(
                        event.kind,
                        crate::brain::BrainEventKind::RunStatusChanged {
                            run_id, status, ..
                        } if run_id == run.run_id && status.is_terminal()
                    ))
                    .count(),
                1
            );

            drop(replacement_events);
            drop(replacement);
            drop(runner);
            shutdown.cancel();
            server_task.await.unwrap().unwrap();
        }));
}

#[test]
fn restored_connectionless_turn_fails_closed_if_it_requests_approval() {
    let result = require_approval_connection(None);
    let error = match result {
        Ok(_) => panic!("connectionless restored turn registered an approval"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("approval audience has no live connection generation"));
}

struct BrainTestDaemon {
    lifecycle: crate::server::BrainLifecycleService,
    runners: crate::server::BrainRunnerBroker,
    connection_id: uuid::Uuid,
}

impl super::finch_ipc_capnp::finch_daemon::Server for BrainTestDaemon {
    fn brain_service(
        self: capnp::capability::Rc<Self>,
        _params: super::finch_ipc_capnp::finch_daemon::BrainServiceParams,
        mut results: super::finch_ipc_capnp::finch_daemon::BrainServiceResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        let service: super::finch_ipc_capnp::brain_service::Client =
            capnp_rpc::new_client(BrainRpcService {
                lifecycle: self.lifecycle.clone(),
                runners: self.runners.clone(),
                connection_id: self.connection_id,
            });
        results.get().set_service(service);
        capnp::capability::Promise::ok(())
    }
}

fn test_approval_audience() -> crate::brain::BrainApprovalAudience {
    crate::brain::BrainApprovalAudience {
        brain_id: crate::brain::BrainId(
            uuid::Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        ),
        brain: "shared".into(),
        attachment_id: crate::brain::AttachmentId(
            uuid::Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
        ),
        subject: "alice@box.local".into(),
        role: crate::brain::AttachmentRole::Driver,
        environment_generation: 3,
    }
}

fn encode_test_packed_delivery(
    mut encoded: capnp::data_list::Builder<'_>,
    records: &[crate::server::RunnerEffectRecord],
) {
    let frames = crate::ipc::codec::encode_packed_delivery_envelopes(records).unwrap();
    encoded.set(0, &frames[0]);
}

fn effect_record() -> crate::server::RunnerEffectRecord {
    crate::server::RunnerEffectRecord {
        execution_id: uuid::Uuid::new_v4(),
        entry: crate::vm::EffectJournalEntry {
            effect: crate::vm::VmSideEffect {
                protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                sequence: 3,
                requirement: crate::vm::CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::SessionEmit,
                    selector: crate::vm::ResourceSelector::None,
                },
                event: crate::vm::HostSideEffect::Emit {
                    text: "done".into(),
                },
                output: Vec::new(),
                origin: crate::vm::SourceOrigin::generated("say"),
            },
            state: crate::vm::EffectJournalState::Acknowledged { values: Vec::new() },
        },
    }
}

#[tokio::test]
async fn runner_lifecycle_capability_rejects_a_replaced_lease() {
    let root = tempfile::tempdir().unwrap().keep();
    let store = crate::brain::BrainStore::with_root("box.local", Some(root));
    let runners = crate::server::BrainRunnerBroker::default();
    let lifecycle = crate::server::BrainLifecycleService::new(
        store.clone(),
        runners.clone(),
        crate::server::BrainApprovalBroker::default(),
    );
    lifecycle.create("shared").await.unwrap();
    let _driver = lifecycle
        .attach(
            "shared",
            "alice",
            crate::brain::AttachmentRole::Driver,
            None,
        )
        .unwrap();
    let environment = store.environment().clone();
    let first = lifecycle
        .acquire_runner("shared", "runner-one", &environment, None, 60_000)
        .unwrap();
    let connection_id = uuid::Uuid::new_v4();
    runners
        .claim_connection_lease(connection_id, "shared", first.lease_id)
        .unwrap();
    let control = BrainRunnerControlImpl {
        lifecycle: lifecycle.clone(),
        runners: runners.clone(),
        connection_id,
        brain: "shared".into(),
        lease_id: first.lease_id,
    };
    control.validate_lease().unwrap();

    lifecycle.release_runner("shared", first.lease_id).unwrap();
    let replacement = lifecycle
        .acquire_runner("shared", "runner-two", &environment, None, 60_000)
        .unwrap();
    assert_ne!(replacement.lease_id, first.lease_id);
    let error = control.validate_lease().unwrap_err();
    assert!(error.to_string().contains("active lease"));
}

#[tokio::test]
async fn disconnected_ipc_connection_rebinds_its_durable_runner_lease() {
    let root = tempfile::tempdir().unwrap().keep();
    let store = crate::brain::BrainStore::with_root("box.local", Some(root));
    let runners = crate::server::BrainRunnerBroker::default();
    let lifecycle = crate::server::BrainLifecycleService::new(
        store.clone(),
        runners.clone(),
        crate::server::BrainApprovalBroker::default(),
    );
    lifecycle.create("shared").await.unwrap();
    let environment = store.environment().clone();
    let subject = "runner@box.local/frontend-stable";

    let first_connection = uuid::Uuid::new_v4();
    runners
        .claim_connection_identity(first_connection, subject)
        .unwrap();
    let first = BrainRpcService {
        lifecycle: lifecycle.clone(),
        runners: runners.clone(),
        connection_id: first_connection,
    };
    let lease = first
        .acquire_connection_runner("shared", subject, &environment, None, 60_000)
        .unwrap();
    runners
        .begin_connection_teardown(first_connection)
        .finish()
        .unwrap();

    let replacement_connection = uuid::Uuid::new_v4();
    runners
        .claim_connection_identity(replacement_connection, subject)
        .unwrap();
    let replacement = BrainRpcService {
        lifecycle,
        runners: runners.clone(),
        connection_id: replacement_connection,
    };
    let renewed = replacement
        .acquire_connection_runner(
            "shared",
            subject,
            &environment,
            Some(lease.lease_id),
            60_000,
        )
        .unwrap();
    assert_eq!(renewed.lease_id, lease.lease_id);

    let (callback_tx, _callback_rx) = tokio::sync::mpsc::unbounded_channel();
    runners
        .register_for_connection(
            replacement_connection,
            "shared",
            renewed.lease_id,
            callback_tx,
        )
        .unwrap();
    assert!(runners.has_registration("shared", renewed.lease_id));
}

#[test]
fn local_initialization_clients_require_their_active_driver_connection() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async {
        let store = crate::brain::BrainStore::with_root("box.local", None);
        let runners = crate::server::BrainRunnerBroker::default();
        let lifecycle = crate::server::BrainLifecycleService::new(
            store,
            runners.clone(),
            crate::server::BrainApprovalBroker::default(),
        );
        let daemon: super::finch_ipc_capnp::finch_daemon::Client =
            capnp_rpc::new_client(BrainTestDaemon {
                lifecycle,
                runners,
                connection_id: uuid::Uuid::new_v4(),
            });
        let ipc = crate::ipc::IpcClient::from_test_client(daemon);
        let target = crate::brain::RemoteBrainTarget::local("shared", "127.0.0.1:1").unwrap();
        let mut driver = crate::brain::AttachedBrainClient::local(target.clone(), ipc.clone());
        driver
            .attach("alice", crate::brain::AttachmentRole::Driver, None)
            .await
            .unwrap();
        let mut events = driver.watch().await.unwrap();
        assert!(matches!(
            events.recv().await.unwrap(),
            crate::brain::BrainWireMessage::Snapshot { .. }
        ));
        assert!(driver
            .schedule_initialization(1_000)
            .await
            .unwrap()
            .module_identity
            .is_some());

        driver.disconnect().await.unwrap();
        assert!(driver.schedule_initialization(2_000).await.is_err());

        let mut consultant = crate::brain::AttachedBrainClient::local(target, ipc);
        consultant
            .attach("bob", crate::brain::AttachmentRole::Consultant, None)
            .await
            .unwrap();
        let mut consultant_events = consultant.watch().await.unwrap();
        assert!(matches!(
            consultant_events.recv().await.unwrap(),
            crate::brain::BrainWireMessage::Snapshot { .. }
        ));
        assert!(consultant.schedule_initialization(3_000).await.is_err());
    }));
}

#[test]
fn runner_turn_result_decodes_ordered_capnp_lifecycle() {
    let expected_effect = effect_record();
    let runtime = crate::runtime::ProgramRuntime::new();
    let checkpoint = runtime
        .revision_history()
        .unwrap()
        .pop()
        .unwrap()
        .checkpoint
        .unwrap();
    let mut message = capnp::message::Builder::new_default();
    {
        let mut result = message.init_root::<super::finch_ipc_capnp::brain_turn_result::Builder>();
        result.set_source("(say \"done\")");
        result.set_language(super::finch_ipc_capnp::ProgramLanguage::Lisp);
        result.set_output("done");
        result.set_runtime_revision(1);
        crate::ipc::codec::encode_continuation_messages(
            result.reborrow().init_continuation_messages(3),
            &[
                crate::providers::Message::with_content(
                    "assistant",
                    vec![
                        crate::providers::ContentBlock::opaque_reasoning("opaque-tool-token"),
                        crate::providers::ContentBlock::ToolUse {
                            id: "tool-1".into(),
                            name: "search_word".into(),
                            input: serde_json::json!({"query":"fib"}),
                        },
                    ],
                ),
                crate::providers::Message::with_content(
                    "user",
                    vec![crate::providers::ContentBlock::tool_result(
                        "tool-1".into(),
                        "found".into(),
                        None,
                    )],
                ),
                crate::providers::Message::with_content(
                    "assistant",
                    vec![
                        crate::providers::ContentBlock::opaque_reasoning("opaque-runner-token"),
                        crate::providers::ContentBlock::text("(say \"done\")"),
                    ],
                ),
            ],
        )
        .unwrap();
        result.set_has_invocation_metadata(true);
        crate::ipc::codec::encode_invocation_metadata(
            result.reborrow().init_invocation_metadata(),
            &crate::providers::InvocationMetadata {
                requested_model: "gpt-5.6".into(),
                resolved_model: "gpt-5.6".into(),
                actual_model: "gpt-5.6-sol".into(),
                input_tokens: Some(5),
                output_tokens: Some(3),
                primary_allowance_used_percent: Some(40.0),
                secondary_allowance_used_percent: None,
            },
        );
        super::encode_checkpoint(result.reborrow().init_checkpoint(), &checkpoint).unwrap();
        result.set_error("");
        crate::ipc::codec::encode_effect_record(
            result.reborrow().init_effect_journal(1).get(0),
            expected_effect.execution_id,
            &expected_effect.entry,
        )
        .unwrap();
        encode_test_packed_delivery(
            result.reborrow().init_delivery(1),
            std::slice::from_ref(&expected_effect),
        );
        let mut events = result.init_turn_events(4);
        let mut call = events.reborrow().get(0);
        call.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::Call);
        call.set_tool_id("tool-1");
        call.set_name("search_word");
        crate::ipc::codec::encode_json_value(
            call.reborrow().init_input(),
            &serde_json::json!({"query": "fib"}),
        )
        .unwrap();
        let mut approval = events.reborrow().get(1);
        approval.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::ApprovalRequested);
        approval.set_approval_id("tool-1");
        approval.set_approval_kind("tool");
        approval.set_subject("search_word");
        encode_approval_audience(
            approval.reborrow().init_approval_audience(),
            &test_approval_audience(),
        );
        crate::ipc::codec::encode_json_value(
            approval.reborrow().init_detail(),
            &serde_json::json!({"input": {"query": "fib"}}),
        )
        .unwrap();
        let mut decision = events.reborrow().get(2);
        decision.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::ApprovalDecided);
        decision.set_approval_id("tool-1");
        crate::ipc::codec::encode_json_value(
            decision.reborrow().init_decision(),
            &serde_json::json!({"choice": "approve_once"}),
        )
        .unwrap();
        let mut tool_result = events.reborrow().get(3);
        tool_result.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::Result);
        tool_result.set_tool_id("tool-1");
        tool_result.set_output("found");
        tool_result.set_is_error(false);
    }

    let reader = message
        .get_root_as_reader::<super::finch_ipc_capnp::brain_turn_result::Reader>()
        .unwrap();
    let decoded = decode_runner_turn_result(Ok(reader)).unwrap();
    assert_eq!(
        decoded.turn_events,
        vec![
            crate::server::RunnerTurnEvent::Call {
                tool_id: "tool-1".into(),
                name: "search_word".into(),
                input: serde_json::json!({"query": "fib"}),
            },
            crate::server::RunnerTurnEvent::ApprovalRequested {
                approval_id: "tool-1".into(),
                approval_kind: "tool".into(),
                subject: "search_word".into(),
                audience: test_approval_audience(),
                detail: serde_json::json!({"input": {"query": "fib"}}),
            },
            crate::server::RunnerTurnEvent::ApprovalDecided {
                approval_id: "tool-1".into(),
                decision: serde_json::json!({"choice": "approve_once"}),
            },
            crate::server::RunnerTurnEvent::Result {
                tool_id: "tool-1".into(),
                output: "found".into(),
                is_error: false,
            },
        ]
    );
    assert_eq!(decoded.effect_journal, vec![expected_effect]);
    assert!(matches!(
        decoded.continuation_messages.last().unwrap().content.as_slice(),
        [
            crate::providers::ContentBlock::OpaqueReasoning { encrypted_content },
            crate::providers::ContentBlock::Text { text },
        ] if encrypted_content == "opaque-runner-token" && text == "(say \"done\")"
    ));
    assert_eq!(
        decoded.invocation_metadata.unwrap().actual_model,
        "gpt-5.6-sol"
    );
}

#[test]
fn runner_turn_error_keeps_partial_lifecycle() {
    let expected_effect = effect_record();
    let mut message = capnp::message::Builder::new_default();
    {
        let mut result = message.init_root::<super::finch_ipc_capnp::brain_turn_result::Builder>();
        result.set_error("provider failed after approval");
        crate::ipc::codec::encode_effect_record(
            result.reborrow().init_effect_journal(1).get(0),
            expected_effect.execution_id,
            &expected_effect.entry,
        )
        .unwrap();
        encode_test_packed_delivery(
            result.reborrow().init_delivery(1),
            std::slice::from_ref(&expected_effect),
        );
        let mut events = result.init_turn_events(1);
        let mut decision = events.reborrow().get(0);
        decision.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::ApprovalDecided);
        decision.set_approval_id("approval-1");
        crate::ipc::codec::encode_json_value(
            decision.reborrow().init_decision(),
            &serde_json::json!({"choice": "deny"}),
        )
        .unwrap();
    }

    let reader = message
        .get_root_as_reader::<super::finch_ipc_capnp::brain_turn_result::Reader>()
        .unwrap();
    let error = decode_runner_turn_result(Ok(reader)).unwrap_err();
    assert_eq!(error.message, "provider failed after approval");
    assert_eq!(
        error.turn_events,
        vec![crate::server::RunnerTurnEvent::ApprovalDecided {
            approval_id: "approval-1".into(),
            decision: serde_json::json!({"choice": "deny"}),
        }]
    );
    assert_eq!(error.effect_journal, vec![expected_effect]);
}

#[test]
fn runner_program_error_keeps_execute_once_effects() {
    let expected_effect = effect_record();
    let mut message = capnp::message::Builder::new_default();
    {
        let mut result =
            message.init_root::<super::finch_ipc_capnp::brain_program_result::Builder>();
        result.set_error("program failed after emit");
        crate::ipc::codec::encode_effect_record(
            result.reborrow().init_effect_journal(1).get(0),
            expected_effect.execution_id,
            &expected_effect.entry,
        )
        .unwrap();
        encode_test_packed_delivery(
            result.reborrow().init_delivery(1),
            std::slice::from_ref(&expected_effect),
        );
    }
    let reader = message
        .get_root_as_reader::<super::finch_ipc_capnp::brain_program_result::Reader>()
        .unwrap();
    let error = decode_runner_program_result(Ok(reader)).unwrap_err();
    assert_eq!(error.message, "program failed after emit");
    assert_eq!(error.effect_journal, vec![expected_effect]);
}

#[test]
fn packed_delivery_on_runner_program_result_must_match_the_journal() {
    let expected_effect = effect_record();
    let mut message = capnp::message::Builder::new_default();
    {
        let mut result =
            message.init_root::<super::finch_ipc_capnp::brain_program_result::Builder>();
        result.set_error("program failed after emit");
        crate::ipc::codec::encode_effect_record(
            result.reborrow().init_effect_journal(1).get(0),
            expected_effect.execution_id,
            &expected_effect.entry,
        )
        .unwrap();
        let frames =
            crate::ipc::codec::encode_packed_delivery_envelopes(&[expected_effect.clone()])
                .unwrap();
        result.reborrow().init_delivery(1).set(0, &frames[0]);
    }
    let reader = message
        .get_root_as_reader::<super::finch_ipc_capnp::brain_program_result::Reader>()
        .unwrap();
    let error = decode_runner_program_result(Ok(reader)).unwrap_err();
    assert_eq!(error.effect_journal, vec![expected_effect.clone()]);

    let mut mismatched = expected_effect.clone();
    mismatched.entry.effect.output = vec![crate::vm::Type::Bytes];
    let mut message = capnp::message::Builder::new_default();
    {
        let mut result =
            message.init_root::<super::finch_ipc_capnp::brain_program_result::Builder>();
        result.set_error("program failed after emit");
        crate::ipc::codec::encode_effect_record(
            result.reborrow().init_effect_journal(1).get(0),
            expected_effect.execution_id,
            &expected_effect.entry,
        )
        .unwrap();
        let frames = crate::ipc::codec::encode_packed_delivery_envelopes(&[mismatched]).unwrap();
        result.reborrow().init_delivery(1).set(0, &frames[0]);
    }
    let reader = message
        .get_root_as_reader::<super::finch_ipc_capnp::brain_program_result::Reader>()
        .unwrap();
    let error = decode_runner_program_result(Ok(reader)).unwrap_err();
    assert!(
        error
            .message
            .contains("packed delivery does not match journal")
            || error
                .to_string()
                .contains("packed delivery does not match journal"),
        "mismatched output row must fail closed, got {}",
        error.message
    );
}

#[test]
fn omitted_packed_delivery_with_a_journal_fails_closed() {
    let expected_effect = effect_record();
    let mut message = capnp::message::Builder::new_default();
    {
        let mut result =
            message.init_root::<super::finch_ipc_capnp::brain_program_result::Builder>();
        result.set_error("program failed after emit");
        crate::ipc::codec::encode_effect_record(
            result.reborrow().init_effect_journal(1).get(0),
            expected_effect.execution_id,
            &expected_effect.entry,
        )
        .unwrap();
    }
    let reader = message
        .get_root_as_reader::<super::finch_ipc_capnp::brain_program_result::Reader>()
        .unwrap();
    let error = decode_runner_program_result(Ok(reader)).unwrap_err();
    assert!(
        error.message.contains("packed delivery omitted")
            || error.to_string().contains("packed delivery omitted"),
        "generation 9 must fail closed when packed delivery is omitted, got {}",
        error.message
    );
}

fn ipc_delivery_envelope(
    execution_id: uuid::Uuid,
    sequence: u64,
    text: &str,
) -> crate::runtime::VmEffectEnvelope {
    crate::runtime::VmEffectEnvelope {
        execution_id,
        effect: crate::vm::VmSideEffect {
            protocol_version: 1,
            sequence,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            output: Vec::new(),
            event: crate::vm::HostSideEffect::Emit { text: text.into() },
            origin: crate::vm::SourceOrigin::generated("ipc-delivery-test"),
        },
    }
}

fn ipc_delivery_envelopes(
    messages: &[crate::runtime::RuntimeApplicationMessage],
) -> Vec<crate::runtime::VmEffectEnvelope> {
    messages
        .iter()
        .map(|message| match message {
            crate::runtime::RuntimeApplicationMessage::Envelope { envelope } => envelope.clone(),
            other => panic!("expected packed envelope, got {other:?}"),
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn register_brain_runner_replays_unacked_packed_delivery_and_ack_clears_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let store =
                crate::brain::BrainStore::with_root("box.local", Some(temp.path().join("brains")));
            store.snapshot("shared").unwrap();
            let execution_id = uuid::Uuid::new_v4();
            let first = ipc_delivery_envelope(execution_id, 0, "one");
            let second = ipc_delivery_envelope(execution_id, 1, "two");
            store
                .record_effect_delivery("shared", &[first.clone(), second.clone()])
                .unwrap();
            let server = std::sync::Arc::new(
                crate::server::AgentServer::for_brain_protocol_test(
                    store.clone(),
                    crate::brain::BrainCredentialAuthority::ephemeral([91; 32]),
                    "test-password".into(),
                    temp.path(),
                )
                .unwrap(),
            );
            let daemon: super::finch_ipc_capnp::finch_daemon::Client = capnp_rpc::new_client(
                FinchDaemonImpl::new(std::sync::Arc::clone(&server), uuid::Uuid::new_v4()),
            );
            let ipc = crate::ipc::IpcClient::from_test_client(daemon);
            let snapshot = ipc.brain_snapshot("shared").await.unwrap();
            let subject = "runner@box.local/frontend-delivery";
            ipc.brain_claim_runner_identity(subject).await.unwrap();
            let lease = ipc
                .brain_acquire_runner("shared", subject, &snapshot.environment, None, 60_000)
                .await
                .unwrap();
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            let bootstrap = ipc
                .register_brain_runner("shared", lease.lease_id, event_tx)
                .await
                .unwrap();
            assert_eq!(
                ipc_delivery_envelopes(&bootstrap.pending_delivery),
                vec![first.clone(), second.clone()],
                "registerBrainRunner must replay the unacknowledged packed suffix"
            );
            assert!(ipc
                .brain_acknowledge_effect_delivery(
                    "shared",
                    lease.lease_id.0,
                    crate::runtime::DeliveryCursor::through(execution_id, 1),
                )
                .await
                .unwrap());
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            let replayed = ipc
                .register_brain_runner("shared", lease.lease_id, event_tx)
                .await
                .unwrap();
            assert!(
                replayed.pending_delivery.is_empty(),
                "ack through the runner consumer must clear pendingDelivery; pending={:?}",
                replayed.pending_delivery
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pending_effect_delivery_survives_observer_disconnect_and_late_completion() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let store = crate::brain::BrainStore::with_root(
                "box.local",
                Some(temp.path().join("brains")),
            );
            store.snapshot("shared").unwrap();
            let execution_id = uuid::Uuid::new_v4();
            let envelope = ipc_delivery_envelope(execution_id, 0, "late");
            store
                .record_effect_delivery("shared", &[envelope.clone()])
                .unwrap();
            let observer = uuid::Uuid::new_v4();
            let server = std::sync::Arc::new(
                crate::server::AgentServer::for_brain_protocol_test(
                    store.clone(),
                    crate::brain::BrainCredentialAuthority::ephemeral([92; 32]),
                    "test-password".into(),
                    temp.path(),
                )
                .unwrap(),
            );
            let first: super::finch_ipc_capnp::finch_daemon::Client =
                capnp_rpc::new_client(FinchDaemonImpl::new(
                    std::sync::Arc::clone(&server),
                    uuid::Uuid::new_v4(),
                ));
            let first_ipc = crate::ipc::IpcClient::from_test_client(first);
            assert_eq!(
                ipc_delivery_envelopes(
                    &first_ipc
                        .brain_pending_effect_delivery("shared", observer)
                        .await
                        .unwrap()
                ),
                vec![envelope.clone()]
            );
            drop(first_ipc);

            let replacement: super::finch_ipc_capnp::finch_daemon::Client =
                capnp_rpc::new_client(FinchDaemonImpl::new(
                    std::sync::Arc::clone(&server),
                    uuid::Uuid::new_v4(),
                ));
            let replacement_ipc = crate::ipc::IpcClient::from_test_client(replacement);
            let pending = replacement_ipc
                .brain_pending_effect_delivery("shared", observer)
                .await
                .unwrap();
            assert_eq!(
                ipc_delivery_envelopes(&pending),
                vec![envelope.clone()],
                "disconnect must not drop unacked delivery; a replacement observer must replay the exact envelope"
            );
            assert!(replacement_ipc
                .brain_acknowledge_effect_delivery(
                    "shared",
                    observer,
                    crate::runtime::DeliveryCursor::through(execution_id, 0),
                )
                .await
                .unwrap());
            assert!(replacement_ipc
                .brain_pending_effect_delivery("shared", observer)
                .await
                .unwrap()
                .is_empty());
        })
        .await;
}

#[test]
fn supervised_ipc_listener_ancestor_swap_never_mutates_replacement_path() {
    if std::env::var("FINCH_BRAIN_TEST_ISOLATED").as_deref() != Ok("1") {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async {
        let proof = crate::brain::isolated_test_proof().unwrap();
        let prepared = super::prepare_ipc_listener().await.unwrap();
        assert!(!prepared.remove_on_shutdown);

        let store = crate::brain::BrainStore::with_root("box.local", Some(proof.root.clone()));
        let server = std::sync::Arc::new(
            crate::server::AgentServer::for_brain_protocol_test(
                store,
                crate::brain::BrainCredentialAuthority::ephemeral([92; 32]),
                "test-password".into(),
                &proof.home,
            )
            .unwrap(),
        );

        let moved = proof.socket_root.with_file_name(format!(
            "{}.moved-{}",
            proof.socket_root.file_name().unwrap().to_string_lossy(),
            uuid::Uuid::new_v4().simple()
        ));
        let replacement = proof.home.join(".finch");
        let sentinel = replacement.join("ipc-swap-sentinel");
        let attacker_socket = replacement.join("daemon.sock");
        std::fs::write(&sentinel, b"outside-must-not-change").unwrap();
        std::fs::write(&attacker_socket, b"not-a-socket").unwrap();
        std::fs::rename(&proof.socket_root, &moved).unwrap();
        std::os::unix::fs::symlink(&replacement, &proof.socket_root).unwrap();

        let shutdown = tokio_util::sync::CancellationToken::new();
        shutdown.cancel();
        super::serve_ipc_listener(server, shutdown, prepared)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"outside-must-not-change"
        );
        assert_eq!(std::fs::read(&attacker_socket).unwrap(), b"not-a-socket");
        assert!(std::fs::symlink_metadata(&attacker_socket)
            .unwrap()
            .file_type()
            .is_file());

        std::fs::remove_file(&proof.socket_root).unwrap();
        std::fs::rename(&moved, &proof.socket_root).unwrap();
        std::fs::remove_file(sentinel).unwrap();
        std::fs::remove_file(attacker_socket).unwrap();
    }));
}

#[tokio::test]
async fn eval_forth_uses_typed_signatures_and_runtime() {
    let (stack, output) = execute_typed_forth_ipc(
        ": double ( S n:int -- S int ! pure ) n n + ; 21 double".to_string(),
    )
    .await
    .unwrap();
    assert_eq!(stack, vec![42]);
    assert!(output.is_empty());

    let error = execute_typed_forth_ipc(": legacy dup * ; 4 legacy".to_string())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("E-FORTH-SIG-001"));
}
