/// Every systemic condition this runner can report must declare itself with
/// `RUNNER_UNAVAILABLE_PREFIX`, so a replay pass aborts instead of paying a
/// round trip per completed run. #254.
///
/// Be exact about what this reaches. It calls the production guard and
/// asserts on the string that guard produces, so dropping the prefix from
/// the guard fails it. It does **not** reach the broker: nothing here calls
/// `try_project_memory`, and deleting the consumer-side `strip_prefix`
/// would leave this green. The consumer is pinned separately by
/// `replay_aborts_when_the_runner_declares_a_systemic_condition`, and the
/// other producer by
/// `broken_runner_connection_declares_itself_unavailable_to_memory_replay`,
/// which does traverse the real forwarding path.
///
/// A full round trip would need an `EventLoop`, and nothing in the
/// repository constructs one outside production — which is why the guard
/// was extracted at all.
#[test]
fn runner_declines_to_project_memory_with_the_systemic_prefix() {
    use crate::server::RUNNER_UNAVAILABLE_PREFIX;

    let cases = [
        // No lease on this Brain.
        (Some("other"), true, true, "does not hold the runner lease"),
        // Lease lapsed.
        (
            Some("shared"),
            false,
            true,
            "does not hold the runner lease",
        ),
        // Memory disabled — an ordinary supported configuration.
        (
            Some("shared"),
            true,
            false,
            "memory is disabled on the environment runner",
        ),
    ];

    for (runner_brain, lease_active, memory_enabled, expected) in cases {
        let error = super::EventLoop::runner_can_project_memory(
            runner_brain,
            lease_active,
            memory_enabled,
            "shared",
        )
        .expect_err("this configuration cannot project memory");
        let message = error.to_string();
        assert!(
            message.starts_with(RUNNER_UNAVAILABLE_PREFIX),
            "a systemic condition must declare itself so replay aborts \
                 instead of retrying every run; got {message:?}"
        );
        assert!(
            message.contains(expected),
            "the reason must survive the prefix; got {message:?}"
        );
        // The broker strips exactly this prefix to build `Unavailable`, so
        // what remains has to be a usable reason rather than an empty
        // string.
        assert!(
            message
                .strip_prefix(RUNNER_UNAVAILABLE_PREFIX)
                .is_some_and(|reason| !reason.trim().is_empty()),
            "stripping the prefix must leave the reason; got {message:?}"
        );
    }

    // The healthy configuration must not be declined.
    super::EventLoop::runner_can_project_memory(Some("shared"), true, true, "shared")
        .expect("a leased runner with memory enabled must project");
}

fn admitting_llm_channel() -> (
    tokio::sync::mpsc::UnboundedSender<super::LlmRequest>,
    tokio::sync::mpsc::UnboundedReceiver<uuid::Uuid>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (observed_tx, observed_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(super::LlmRequest::Query {
            id,
            admission,
            admission_ready,
            spawned,
            publication,
            ..
        }) = rx.recv().await
        {
            if let Some(ready) = admission_ready {
                let _ = ready.send(());
            }
            if let Some(admission) = admission {
                if admission.await.is_err() {
                    continue;
                }
            }
            if let Some(spawned) = spawned {
                let _ = spawned.send(());
            }
            if let Some(publication) = publication {
                if publication.await.is_err() {
                    continue;
                }
            }
            let _ = observed_tx.send(id);
        }
    });
    (tx, observed_rx)
}

#[tokio::test]
async fn continuation_is_admitted_exactly_once_for_one_complete_validated_round() {
    use crate::claude::{ContentBlock, Message};
    use crate::cli::conversation::ToolRoundProgress;

    let query_id = uuid::Uuid::new_v4();
    let mut conversation =
        crate::cli::conversation::ConversationHistory::with_limits(2, usize::MAX);
    conversation.add_user_message("older user".to_string());
    conversation.add_assistant_message("older assistant".to_string());
    let token = conversation
        .stage_assistant(
            query_id,
            Message {
                role: "assistant".into(),
                content: ["A", "B"]
                    .into_iter()
                    .map(|id| ContentBlock::ToolUse {
                        id: id.into(),
                        name: "Read".into(),
                        input: serde_json::json!({}),
                    })
                    .collect(),
            },
        )
        .unwrap();
    let (llm_tx, mut observed_rx) = admitting_llm_channel();

    conversation
        .record_tool_result(query_id, token, "A", &Ok("a".into()))
        .unwrap();
    let conversation = std::sync::Arc::new(tokio::sync::RwLock::new(conversation));
    assert!(
        super::commit_tool_round_and_continue(&conversation, query_id, token, &llm_tx, None)
            .await
            .is_err()
    );
    assert!(
        observed_rx.try_recv().is_err(),
        "incomplete round must admit zero continuations"
    );

    assert_eq!(
        conversation
            .write()
            .await
            .record_tool_result(query_id, token, "B", &Ok("b".into()))
            .unwrap(),
        ToolRoundProgress::Complete
    );
    let directory = tempfile::tempdir().unwrap();
    let checkpoint = directory.path().join("session.json");
    super::commit_tool_round_and_continue(
        &conversation,
        query_id,
        token,
        &llm_tx,
        Some(&checkpoint),
    )
    .await
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(query_id));
    let restored = crate::cli::conversation::ConversationHistory::load(&checkpoint).unwrap();
    assert_eq!(restored.get_messages().len(), 2);
    assert_eq!(
        serde_json::to_value(restored.get_messages()).unwrap(),
        serde_json::to_value(conversation.read().await.get_messages()).unwrap(),
        "durable bytes must contain the exact finalized/trimmed history"
    );
    assert!(
        super::commit_tool_round_and_continue(&conversation, query_id, token, &llm_tx, None)
            .await
            .is_err()
    );
    assert!(
        observed_rx.try_recv().is_err(),
        "completed round must admit only one continuation"
    );
}

#[tokio::test]
async fn closed_worker_leaves_complete_round_staged_and_provider_invisible() {
    use crate::claude::{ContentBlock, Message};
    let query_id = uuid::Uuid::new_v4();
    let mut history = crate::cli::conversation::ConversationHistory::new();
    let token = history
        .stage_assistant(
            query_id,
            Message {
                role: "assistant".into(),
                content: vec![ContentBlock::ToolUse {
                    id: "A".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                }],
            },
        )
        .unwrap();
    history
        .record_tool_result(query_id, token, "A", &Ok("a".into()))
        .unwrap();
    let conversation = std::sync::Arc::new(tokio::sync::RwLock::new(history));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    drop(rx);

    assert_eq!(
        super::commit_tool_round_and_continue(&conversation, query_id, token, &tx, None).await,
        Err(crate::cli::conversation::ToolRoundError::ContinuationUnavailable)
    );
    assert!(conversation.read().await.get_messages().is_empty());
    assert!(conversation
        .read()
        .await
        .completed_tool_results(query_id, token)
        .is_ok());
}

#[tokio::test]
async fn worker_exit_after_commit_rolls_complete_pair_back_to_stage() {
    use crate::claude::{ContentBlock, Message};
    let query_id = uuid::Uuid::new_v4();
    let mut history = crate::cli::conversation::ConversationHistory::new();
    let token = history
        .stage_assistant(
            query_id,
            Message {
                role: "assistant".into(),
                content: vec![ContentBlock::ToolUse {
                    id: "A".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                }],
            },
        )
        .unwrap();
    history
        .record_tool_result(query_id, token, "A", &Ok("a".into()))
        .unwrap();
    let conversation = std::sync::Arc::new(tokio::sync::RwLock::new(history));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        if let Some(super::LlmRequest::Query {
            admission,
            admission_ready,
            spawned,
            publication,
            ..
        }) = rx.recv().await
        {
            let _ = admission_ready.unwrap().send(());
            let _ = admission.unwrap().await;
            drop(spawned);
            drop(publication);
        }
    });

    let directory = tempfile::tempdir().unwrap();
    let checkpoint = directory.path().join("session.json");
    conversation.read().await.save(&checkpoint).unwrap();
    assert_eq!(
        super::commit_tool_round_and_continue(
            &conversation,
            query_id,
            token,
            &tx,
            Some(&checkpoint),
        )
        .await,
        Err(crate::cli::conversation::ToolRoundError::ContinuationUnavailable)
    );
    assert!(conversation.read().await.get_messages().is_empty());
    let restored = crate::cli::conversation::ConversationHistory::load(&checkpoint).unwrap();
    assert!(restored.get_messages().is_empty());
    assert!(conversation
        .read()
        .await
        .completed_tool_results(query_id, token)
        .is_ok());
}

#[tokio::test]
async fn checkpoint_failure_revokes_spawned_continuation_and_restores_live_history() {
    use crate::claude::{ContentBlock, Message};
    let query_id = uuid::Uuid::new_v4();
    let mut history = crate::cli::conversation::ConversationHistory::new();
    let token = history
        .stage_assistant(
            query_id,
            Message {
                role: "assistant".into(),
                content: vec![ContentBlock::ToolUse {
                    id: "A".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                }],
            },
        )
        .unwrap();
    history
        .record_tool_result(query_id, token, "A", &Ok("a".into()))
        .unwrap();
    let conversation = std::sync::Arc::new(tokio::sync::RwLock::new(history));
    let (tx, mut observed_rx) = admitting_llm_channel();
    let directory = tempfile::tempdir().unwrap();

    assert!(matches!(
        super::commit_tool_round_and_continue(
            &conversation,
            query_id,
            token,
            &tx,
            Some(directory.path()),
        )
        .await,
        Err(crate::cli::conversation::ToolRoundError::PersistenceUnavailable(_))
    ));
    assert!(conversation.read().await.get_messages().is_empty());
    assert!(conversation
        .read()
        .await
        .completed_tool_results(query_id, token)
        .is_ok());
    assert!(observed_rx.try_recv().is_err());
}

#[test]
fn bare_brain_attach_routes_only_to_local_ipc() {
    assert_eq!(
        super::brain_attachment_route("review", None).unwrap(),
        super::BrainAttachmentRoute::LocalIpc {
            brain: "review".into()
        }
    );
}

#[test]
fn remote_brain_attach_requires_the_invitation_join_path() {
    let error = super::brain_attachment_route("review@workstation.local", None).unwrap_err();
    assert!(error.to_string().contains("/brain join"));

    let route = super::brain_attachment_route(
        "review@workstation.local:19436",
        Some("finch-brain-invite-v1.payload.signature".into()),
    )
    .unwrap();
    assert!(matches!(
        route,
        super::BrainAttachmentRoute::RemoteInvitation { target, invitation }
            if target.address == "workstation.local:19436"
                && invitation == "finch-brain-invite-v1.payload.signature"
    ));
}

#[test]
fn invitation_join_rejects_a_bare_local_target() {
    let error = super::brain_attachment_route(
        "review",
        Some("finch-brain-invite-v1.payload.signature".into()),
    )
    .unwrap_err();
    assert!(error.to_string().contains("NAME@MACHINE[:PORT]"));
}

#[tokio::test]
async fn named_brain_schedule_effect_uses_the_run_scoped_control_proxy() {
    let runtime = crate::runtime::ProgramRuntime::new();
    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
    let schedule_id = crate::brain::store::ScheduleId(uuid::Uuid::new_v4());
    tokio::spawn(async move {
        let crate::server::RunnerProgramControlRequest::CreateSchedule {
            language,
            source,
            grant_ceiling,
            next_due_ms,
            interval_ms,
            delivery_policy,
            response_tx,
        } = control_rx.recv().await.unwrap()
        else {
            panic!("expected schedule creation")
        };
        assert_eq!(language, crate::brain::store::ProgramLanguage::Lisp);
        assert_eq!(source, "(say \"later\")");
        assert!(grant_ceiling.is_pure());
        assert_eq!(next_due_ms, 1_770_000_000_000);
        assert_eq!(interval_ms, None);
        assert_eq!(
            delivery_policy,
            crate::brain::store::BrainScheduleDeliveryPolicy::Coalesce
        );
        response_tx
            .send(Ok(crate::brain::store::BrainSchedule {
                schedule_id,
                initiating_attachment_id: crate::brain::store::AttachmentId(uuid::Uuid::new_v4()),
                created_by: "alice".into(),
                grant_ceiling,
                language,
                source,
                next_due_ms,
                interval_ms,
                delivery_policy,
                module_identity: None,
                active: true,
            }))
            .unwrap();
    });
    let effect = crate::vm::VmSideEffect {
        protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
        sequence: 1,
        requirement: crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ScheduleCreate,
            selector: crate::vm::ResourceSelector::Schedule { policy: None },
        },
        event: crate::vm::HostSideEffect::Request {
            arguments: vec![
                crate::vm::TypedValue::String("(say \"later\")".into()),
                crate::vm::TypedValue::Int(1_770_000_000),
            ],
        },
        output: vec![crate::vm::Type::Resource("schedule".into())],
        origin: crate::vm::SourceOrigin::generated("schedule-create"),
    };

    let values = super::execute_named_brain_schedule_effect(
        &runtime,
        &control_tx,
        crate::brain::store::ProgramLanguage::Lisp,
        Some(&crate::vm::EffectSet::pure()),
        &effect,
    )
    .await
    .unwrap();
    assert!(matches!(
        values.as_slice(),
        [crate::vm::TypedValue::Resource { kind, handle, .. }]
            if kind == "schedule" && handle == &schedule_id.0.to_string()
    ));
}

#[test]
fn approval_audit_value_preserves_the_decision_scope() {
    let decision = super::confirmation_audit_value(
        &crate::cli::repl_event::events::ConfirmationResult::ApproveOnce,
    );
    assert_eq!(decision, serde_json::json!({"choice": "approve_once"}));
}

#[test]
fn remote_tool_approval_round_trips_edited_input() {
    let tool_use = crate::tools::types::ToolUse {
        id: "tool-1".into(),
        name: "edit".into(),
        input: serde_json::json!({"path": "src/main.rs", "new_string": "old"}),
    };
    let edited = serde_json::json!({"path": "src/main.rs", "new_string": "new"});
    let audit = super::confirmation_audit_value(
        &crate::cli::repl_event::events::ConfirmationResult::ApproveWithInput(edited.clone()),
    );

    assert!(matches!(
        super::confirmation_from_audit_value(&audit, &tool_use).unwrap(),
        crate::cli::repl_event::events::ConfirmationResult::ApproveWithInput(input)
            if input == edited
    ));
}

use super::*;

#[test]
fn initialization_command_distinguishes_completed_one_shot() {
    let store = crate::brain::store::BrainStore::with_root("box.local", None);
    let attachment = store
        .attach(
            "shared",
            "alice",
            crate::brain::store::AttachmentRole::Driver,
            None,
        )
        .unwrap();
    let connection_id = attachment.connection_id.unwrap();
    store
        .activate_connection("shared", attachment.attachment_id, connection_id)
        .unwrap();
    let schedule = store
        .schedule_initialization("shared", attachment.attachment_id, connection_id, 10)
        .unwrap();
    assert!(initialization_schedule_message(&schedule, None).contains("scheduled"));
    let mut completed = schedule;
    completed.active = false;
    let message = initialization_schedule_message(
        &completed,
        Some(crate::brain::store::BrainRunStatus::Completed),
    );
    assert!(message.contains("already completed"));
    assert!(!message.contains(" scheduled as "));
}

fn brain_event(
    seq: u64,
    sender: &str,
    kind: crate::brain::store::BrainEventKind,
) -> crate::brain::store::BrainEvent {
    crate::brain::store::BrainEvent {
        schema_version: 1,
        brain_id: crate::brain::store::BrainId(uuid::Uuid::nil()),
        seq,
        environment_generation: 1,
        sender: sender.into(),
        created_ms: 0,
        run_id: None,
        mutation: None,
        kind,
    }
}

#[test]
fn snapshot_replay_keeps_conversation_and_hides_presence_churn() {
    use crate::brain::store::{AttachmentId, AttachmentRole, BrainEventKind, ConnectionId};

    let prompt = brain_event(
        1,
        "alice",
        BrainEventKind::Prompt {
            text: "hello".into(),
        },
    );
    let attached = brain_event(
        2,
        "daemon",
        BrainEventKind::ClientAttached {
            attachment_id: AttachmentId(uuid::Uuid::new_v4()),
            connection_id: ConnectionId(uuid::Uuid::new_v4()),
            subject: "alice".into(),
            role: AttachmentRole::Driver,
        },
    );

    assert!(replay_event_belongs_in_transcript(&prompt));
    assert!(!replay_event_belongs_in_transcript(&attached));
}

#[test]
fn brain_projection_suppresses_snapshot_live_overlap() {
    let brain_id = crate::brain::store::BrainId(uuid::Uuid::new_v4());
    let mut revisions = std::collections::HashMap::new();

    assert!(advance_brain_projection_revision(
        &mut revisions,
        brain_id,
        12
    ));
    assert!(!advance_brain_projection_revision(
        &mut revisions,
        brain_id,
        12
    ));
    assert!(!advance_brain_projection_revision(
        &mut revisions,
        brain_id,
        11
    ));
}

#[test]
fn brain_projection_keeps_later_transitions_and_brains_independent() {
    let first = crate::brain::store::BrainId(uuid::Uuid::new_v4());
    let second = crate::brain::store::BrainId(uuid::Uuid::new_v4());
    let mut revisions = std::collections::HashMap::new();

    assert!(advance_brain_projection_revision(&mut revisions, first, 20));
    assert!(advance_brain_projection_revision(&mut revisions, first, 21));
    assert!(advance_brain_projection_revision(&mut revisions, second, 1));
}

#[test]
fn canonical_brain_context_projects_conversation_without_program_source() {
    use crate::brain::store::{BrainEventKind, ProgramLanguage};
    use crate::cli::status_bar::{StatusBar, StatusLineType};

    let events = vec![
        brain_event(
            1,
            "alice",
            BrainEventKind::Prompt {
                text: "please compute forty two squared".into(),
            },
        ),
        brain_event(
            2,
            "model",
            BrainEventKind::Program {
                language: ProgramLanguage::Lisp,
                source: "(say \"1764\")".into(),
            },
        ),
        brain_event(
            3,
            "model",
            BrainEventKind::Result {
                request_seq: 1,
                output: "1764".into(),
                error: None,
                continuation_messages: Vec::new(),
                invocation_metadata: None,
            },
        ),
    ];
    let status = StatusBar::new();

    super::project_brain_context(&status, &events, 2, None);

    let lines = status
        .get_lines()
        .into_iter()
        .filter(|line| matches!(line.line_type, StatusLineType::BrainContextLine(_)))
        .map(|line| line.content)
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec![
            "💬 alice: please compute forty two squared",
            "   └─ now: model: 1764",
        ]
    );

    super::project_brain_context(&status, &[], 2, None);
    assert!(status
        .get_lines()
        .iter()
        .all(|line| !matches!(line.line_type, StatusLineType::BrainContextLine(_))));
}

#[test]
fn canonical_brain_context_ignores_failed_results_and_bounds_text() {
    use crate::brain::store::BrainEventKind;
    use crate::cli::status_bar::{StatusBar, StatusLineType};

    let events = vec![
        brain_event(
            1,
            "alice",
            BrainEventKind::Prompt {
                text: "a".repeat(100),
            },
        ),
        brain_event(
            2,
            "model",
            BrainEventKind::Result {
                request_seq: 1,
                output: "partial output".into(),
                error: Some("provider failed".into()),
                continuation_messages: Vec::new(),
                invocation_metadata: None,
            },
        ),
    ];
    let status = StatusBar::new();

    super::project_brain_context(&status, &events, 4, None);

    let lines = status
        .get_lines()
        .into_iter()
        .filter(|line| matches!(line.line_type, StatusLineType::BrainContextLine(_)))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].content.starts_with("   └─ now: alice: "));
    assert!(lines[0].content.ends_with('…'));
    assert!(!lines[0].content.contains("partial output"));
}

#[test]
fn canonical_brain_context_excludes_correlated_speculative_output() {
    use crate::brain::store::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };
    let run_id = RunId(uuid::Uuid::new_v4());
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Speculative,
        parent_run_id: None,
        request_seq: 1,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "alice".into(),
        status: BrainRunStatus::Completed,
        started_ms: 1,
        updated_ms: 2,
        detail: None,
    };
    let mut request = brain_event(
        1,
        "alice",
        BrainEventKind::SpeculativePrompt {
            text: "hidden helper prompt".into(),
        },
    );
    request.run_id = Some(run_id);
    let mut started = brain_event(2, "alice", BrainEventKind::RunStarted { run });
    started.run_id = Some(run_id);
    let mut result = brain_event(
        3,
        "daemon",
        BrainEventKind::Result {
            request_seq: 1,
            output: "hidden helper output".into(),
            error: None,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
    );
    result.run_id = Some(run_id);
    let ordinary = brain_event(
        4,
        "bob",
        BrainEventKind::ParticipantMessage {
            text: "visible collaboration".into(),
        },
    );

    assert_eq!(
        projected_brain_context_lines(&[request, started, result, ordinary], 4, None),
        vec!["bob: visible collaboration"]
    );
}

#[test]
fn snapshot_groups_speculative_lifecycle_program_and_result_by_exact_run_id() {
    use crate::brain::store::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, ProgramLanguage,
        RunId,
    };
    let run_id = RunId(uuid::Uuid::new_v4());
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Speculative,
        parent_run_id: None,
        request_seq: 1,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "alice".into(),
        status: BrainRunStatus::QueuedForEnvironment,
        started_ms: 1,
        updated_ms: 1,
        detail: None,
    };
    let kinds = vec![
        BrainEventKind::SpeculativePrompt {
            text: "probe".into(),
        },
        BrainEventKind::RunStarted { run },
        BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Running,
            detail: None,
        },
        BrainEventKind::Program {
            language: ProgramLanguage::Lisp,
            source: "(say \"probe\")".into(),
        },
        BrainEventKind::Result {
            request_seq: 4,
            output: "probe".into(),
            error: None,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
        BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Completed,
            detail: None,
        },
    ];
    let events = kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            let mut event = brain_event(index as u64 + 1, "daemon", kind);
            event.run_id = Some(run_id);
            event
        })
        .collect::<Vec<_>>();

    assert_eq!(
        projected_brain_run_groups(&events),
        vec![BrainRunGroupProjection {
            run_id,
            kind: BrainRunKind::Speculative,
            status: BrainRunStatus::Completed,
            event_seqs: vec![1, 2, 3, 4, 5, 6],
        }]
    );
    assert_eq!(
        brain_run_group_label(run_id, Some(BrainRunKind::Speculative)),
        format!("Speculative run {}", run_id.0)
    );
}

#[test]
fn pre_inference_brain_provider_failure_is_activity_not_tool_group() {
    use crate::brain::store::{BrainEventKind, BrainRunKind, BrainRunStatus, RunId};
    use crate::cli::messages::{Message, TranscriptRowKind};

    let output =
        crate::cli::output_manager::OutputManager::new(crate::config::ColorScheme::default());
    output.disable_stdout();
    let run_id = RunId(uuid::Uuid::new_v4());
    let mut projections = std::collections::HashMap::new();
    super::ensure_remote_brain_run_projection(
        &output,
        &mut projections,
        run_id,
        Some(BrainRunKind::Speculative),
        BrainRunStatus::Running,
    );

    let mut result = brain_event(
        1,
        "provider",
        BrainEventKind::Result {
            request_seq: 1,
            output: String::new(),
            error: Some("catalog unavailable".into()),
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
    );
    result.run_id = Some(run_id);
    assert!(super::project_remote_brain_run_event(
        &output,
        &mut projections,
        &result,
    ));

    let mut terminal = brain_event(
        2,
        "daemon",
        BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Failed,
            detail: Some("catalog unavailable".into()),
        },
    );
    terminal.run_id = Some(run_id);
    assert!(super::project_remote_brain_run_event(
        &output,
        &mut projections,
        &terminal,
    ));

    let unit = projections.get(&run_id).unwrap().unit.clone();
    let projected = unit
        .transcript_row(&crate::config::ColorScheme::default())
        .unwrap();
    assert_eq!(projected.kind, TranscriptRowKind::Activity);
    assert!(projected.label.contains("Speculative run"));
    assert!(projected
        .label
        .contains("status failed: catalog unavailable"));
    assert!(!projected.label.contains("Tools"));
    assert!(!projected.label.contains("calls"));
    assert_eq!(projected.children.len(), 2);
    assert!(projected
        .children
        .iter()
        .all(|row| row.kind == TranscriptRowKind::Activity));
    assert!(projected.children[0].label.contains("status"));
    assert!(projected.children[1].label.starts_with("result"));

    let canonical = unit.complete_transcript(&crate::config::ColorScheme::default());
    assert!(canonical.contains("Speculative run"));
    assert!(canonical.contains("result"));
    assert!(canonical.contains("catalog unavailable"));
}

#[test]
fn named_brain_run_preserves_tool_semantics_inside_activity_group() {
    use crate::brain::store::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };
    use crate::cli::messages::{Message, TranscriptRowKind};

    let output =
        crate::cli::output_manager::OutputManager::new(crate::config::ColorScheme::default());
    output.disable_stdout();
    let run_id = RunId(uuid::Uuid::new_v4());
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Speculative,
        parent_run_id: None,
        request_seq: 1,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "alice".into(),
        status: BrainRunStatus::Running,
        started_ms: 1,
        updated_ms: 1,
        detail: None,
    };
    let kinds = [
        BrainEventKind::RunStarted { run },
        BrainEventKind::ToolCall {
            request_seq: 1,
            tool_id: "tool-1".into(),
            name: "read_cache".into(),
            input: serde_json::json!({"key": "alpha"}),
        },
        BrainEventKind::ToolResult {
            request_seq: 1,
            tool_id: "tool-1".into(),
            output: "cache hit\nvalue=7".into(),
            is_error: false,
        },
        BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Completed,
            detail: None,
        },
    ];
    let mut projections = std::collections::HashMap::new();
    for (index, kind) in kinds.into_iter().enumerate() {
        let mut event = brain_event(index as u64 + 1, "daemon", kind);
        event.run_id = Some(run_id);
        assert!(super::project_remote_brain_run_event(
            &output,
            &mut projections,
            &event,
        ));
    }

    let unit = projections.get(&run_id).unwrap().unit.clone();
    let projected = unit
        .transcript_row(&crate::config::ColorScheme::default())
        .unwrap();
    assert_eq!(projected.kind, TranscriptRowKind::Activity);
    assert!(!projected.default_expanded);
    assert!(!projected.label.contains("Tools"));
    assert_eq!(projected.children.len(), 2);

    let status = &projected.children[0];
    assert_eq!(status.kind, TranscriptRowKind::Activity);
    assert_eq!(status.id.message_id, unit.id());
    assert_eq!(status.id.path, vec![1, 0]);

    let tool = &projected.children[1];
    assert_eq!(tool.kind, TranscriptRowKind::ToolCall);
    assert_eq!(tool.id.message_id, unit.id());
    assert_eq!(tool.id.path, vec![1, 1]);
    assert_eq!(tool.children.len(), 2);
    assert_eq!(tool.children[0].kind, TranscriptRowKind::Input);
    assert_eq!(tool.children[0].id.path, vec![1, 1, 0]);
    assert_eq!(tool.children[1].kind, TranscriptRowKind::ToolOutput);
    assert_eq!(tool.children[1].id.path, vec![1, 1, 1]);
    assert!(tool.children[1].body.iter().any(|line| line == "value=7"));

    let canonical = unit.complete_transcript(&crate::config::ColorScheme::default());
    assert!(canonical.contains("read_cache"));
    assert!(canonical.contains("cache hit"));
    assert!(canonical.contains("value=7"));
}

#[test]
fn snapshot_first_home_reconnect_reconciles_one_complete_work_unit() {
    use crate::brain::store::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, ProgramLanguage,
        RunId,
    };

    let output =
        crate::cli::output_manager::OutputManager::new(crate::config::ColorScheme::default());
    output.disable_stdout();
    let run_id = RunId(uuid::Uuid::new_v4());
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Speculative,
        parent_run_id: None,
        request_seq: 1,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "alice".into(),
        status: BrainRunStatus::QueuedForEnvironment,
        started_ms: 1,
        updated_ms: 1,
        detail: None,
    };
    let kinds = vec![
        BrainEventKind::SpeculativePrompt {
            text: "inspect the cache".into(),
        },
        BrainEventKind::RunStarted { run },
        BrainEventKind::ToolCall {
            request_seq: 1,
            tool_id: "tool-1".into(),
            name: "read_cache".into(),
            input: serde_json::json!({"key": "alpha"}),
        },
        BrainEventKind::ToolResult {
            request_seq: 1,
            tool_id: "tool-1".into(),
            output: "cache hit\nvalue=7".into(),
            is_error: false,
        },
        BrainEventKind::ApprovalRequested {
            request_seq: 1,
            approval_id: "approval-1".into(),
            approval_kind: "tool".into(),
            subject: "write_cache".into(),
            audience: None,
            detail: serde_json::json!({"key": "beta"}),
        },
        BrainEventKind::ApprovalDecided {
            request_seq: 1,
            approval_id: "approval-1".into(),
            decision: serde_json::json!({"choice": "approve_once"}),
        },
        BrainEventKind::Program {
            language: ProgramLanguage::Lisp,
            source: "(say \"cache checked\")".into(),
        },
        BrainEventKind::Result {
            request_seq: 7,
            output: "cache checked".into(),
            error: None,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
        BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Completed,
            detail: None,
        },
    ];
    let events = kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            let sender = if matches!(kind, BrainEventKind::Program { .. }) {
                "provider"
            } else {
                "daemon"
            };
            let mut event = brain_event(index as u64 + 1, sender, kind);
            event.run_id = Some(run_id);
            event
        })
        .collect::<Vec<_>>();
    let mut projections = std::collections::HashMap::new();
    let local_unit = super::ensure_remote_brain_run_projection(
        &output,
        &mut projections,
        run_id,
        Some(BrainRunKind::Speculative),
        BrainRunStatus::Running,
    )
    .unit
    .clone();
    let tool_row = local_unit.add_row("read_cache {\"key\":\"alpha\"}");
    local_unit.complete_row_with_body(tool_row, "cache hit", vec!["value=7".to_string()]);
    let approval_row =
        local_unit.add_row("approval (tool) for legacy audience unspecified: write_cache");
    local_unit.complete_row(approval_row, "approve_once by daemon");
    local_unit.set_program_source("lisp");
    local_unit.set_response("(say \"cache checked\")");
    local_unit.set_complete();
    let transient_output_unit = output.start_work_unit("VM program output");
    transient_output_unit.set_program_output();
    transient_output_unit.set_response("cache checked");
    transient_output_unit.set_complete();
    assert_eq!(output.get_messages().len(), 2);
    let mut local_projections = std::collections::VecDeque::from([LocalBrainProjection {
        run_id,
        source: "(say \"cache checked\")".into(),
        output: "cache checked".into(),
        tool_ids: std::collections::HashSet::from(["tool-1".into()]),
        approval_ids: std::collections::HashSet::from(["approval-1".into()]),
        program_seq: None,
        transient_output_unit: Some(transient_output_unit),
        failed: false,
    }]);

    // No live canonical events arrive. A replacement snapshot containing
    // the acknowledged history must reconcile the local rows, adopt the
    // durable Result, and retire transient VM output by itself.
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
    );
    assert!(local_projections.is_empty());

    let messages = output.get_messages();
    assert_eq!(messages.len(), 1);
    let rendered = messages[0].format(&crate::config::ColorScheme::default());
    for expected in [
        &format!("Speculative run {}", run_id.0),
        "inspect the cache",
        "read_cache",
        "cache hit",
        "approval (tool)",
        "approve_once by daemon",
        "program (lisp)",
        "(say \"cache checked\")",
        "cache checked",
        "completed",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?} from projected WorkUnit:\n{rendered}"
        );
    }
    assert_eq!(rendered.matches("inspect the cache").count(), 1);
    assert_eq!(rendered.matches("read_cache").count(), 1);
    assert_eq!(rendered.matches("approval (tool)").count(), 1);
    assert_eq!(rendered.matches("program (lisp)").count(), 1);
    assert_eq!(rendered.matches("result").count(), 1);
}

#[test]
fn named_brain_continuation_preserves_multi_text_and_opaque_order() {
    let initial = crate::claude::Message::user("prompt");
    let continuation = crate::claude::Message::with_content(
        "assistant",
        vec![
            crate::claude::ContentBlock::text("(say \""),
            crate::claude::ContentBlock::opaque_reasoning("opaque-between-text"),
            crate::claude::ContentBlock::text("done\")"),
        ],
    );
    let (source, language, captured) =
        super::named_brain_wire_source(vec![initial, continuation.clone()], 1).unwrap();
    assert_eq!(source, "(say \"done\")");
    assert_eq!(language, crate::brain::store::ProgramLanguage::Lisp);
    assert_eq!(
        serde_json::to_value(captured).unwrap(),
        serde_json::to_value(vec![continuation]).unwrap()
    );
}

#[test]
fn missing_final_wire_after_home_tool_rounds_reconciles_durable_error() {
    use crate::brain::store::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };

    let output =
        crate::cli::output_manager::OutputManager::new(crate::config::ColorScheme::default());
    output.disable_stdout();
    let run_id = RunId(uuid::Uuid::new_v4());
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Speculative,
        parent_run_id: None,
        request_seq: 1,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "alice".into(),
        status: BrainRunStatus::Running,
        started_ms: 1,
        updated_ms: 1,
        detail: None,
    };
    let mut projections = std::collections::HashMap::new();
    let local_unit = super::ensure_remote_brain_run_projection(
        &output,
        &mut projections,
        run_id,
        Some(BrainRunKind::Speculative),
        BrainRunStatus::Running,
    )
    .unit
    .clone();
    for (tool, summary) in [("tool-one", "first ok"), ("tool-two", "second ok")] {
        let row = local_unit.add_row(tool);
        local_unit.complete_row(row, summary);
    }
    let approval_row = local_unit.add_row("approval (tool) for write_cache");
    local_unit.complete_row(approval_row, "approve_once by daemon");
    let transient_output_unit = output.start_work_unit("VM program output");
    transient_output_unit.set_program_output();
    transient_output_unit.set_response("named Brain turn produced no wire source");
    transient_output_unit.set_failed();
    assert_eq!(output.get_messages().len(), 2);

    let kinds = vec![
        BrainEventKind::RunStarted { run },
        BrainEventKind::ToolCall {
            request_seq: 1,
            tool_id: "tool-1".into(),
            name: "tool-one".into(),
            input: serde_json::json!({}),
        },
        BrainEventKind::ToolResult {
            request_seq: 1,
            tool_id: "tool-1".into(),
            output: "first ok".into(),
            is_error: false,
        },
        BrainEventKind::ToolCall {
            request_seq: 1,
            tool_id: "tool-2".into(),
            name: "tool-two".into(),
            input: serde_json::json!({}),
        },
        BrainEventKind::ToolResult {
            request_seq: 1,
            tool_id: "tool-2".into(),
            output: "second ok".into(),
            is_error: false,
        },
        BrainEventKind::ApprovalRequested {
            request_seq: 1,
            approval_id: "approval-1".into(),
            approval_kind: "tool".into(),
            subject: "write_cache".into(),
            audience: None,
            detail: serde_json::json!({}),
        },
        BrainEventKind::ApprovalDecided {
            request_seq: 1,
            approval_id: "approval-1".into(),
            decision: serde_json::json!({"choice": "approve_once"}),
        },
        BrainEventKind::Result {
            request_seq: 1,
            output: String::new(),
            error: Some("named Brain turn produced no wire source".into()),
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
        BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Failed,
            detail: None,
        },
    ];
    let events = kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            let mut event = brain_event(index as u64 + 1, "daemon", kind);
            event.run_id = Some(run_id);
            event
        })
        .collect::<Vec<_>>();
    let failed_turn_events = vec![
        crate::server::RunnerTurnEvent::Call {
            tool_id: "tool-1".into(),
            name: "tool-one".into(),
            input: serde_json::json!({}),
        },
        crate::server::RunnerTurnEvent::Result {
            tool_id: "tool-1".into(),
            output: "first ok".into(),
            is_error: false,
        },
        crate::server::RunnerTurnEvent::Call {
            tool_id: "tool-2".into(),
            name: "tool-two".into(),
            input: serde_json::json!({}),
        },
        crate::server::RunnerTurnEvent::Result {
            tool_id: "tool-2".into(),
            output: "second ok".into(),
            is_error: false,
        },
        crate::server::RunnerTurnEvent::ApprovalDecided {
            approval_id: "approval-1".into(),
            decision: serde_json::json!({"choice": "approve_once"}),
        },
    ];
    let mut local_projections = std::collections::VecDeque::new();
    let assembly_result = super::assemble_named_brain_turn(
        &mut local_projections,
        run_id,
        Ok(Vec::new()),
        &crate::runtime::ProgramRuntime::new(),
        String::new(),
        failed_turn_events,
        Vec::new(),
        None,
        Some(transient_output_unit),
        None,
        0,
    );
    assert_eq!(
        assembly_result.unwrap_err().message,
        "named Brain turn produced no wire source"
    );
    assert_eq!(local_projections[0].tool_ids.len(), 2);
    assert_eq!(local_projections[0].approval_ids.len(), 1);

    for event in &events {
        assert!(super::project_remote_brain_live_run_event(
            &output,
            &mut projections,
            &mut local_projections,
            true,
            event,
        ));
    }
    assert!(local_projections.is_empty());
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
    );

    let messages = output.get_messages();
    assert_eq!(messages.len(), 1);
    let rendered = messages[0].format(&crate::config::ColorScheme::default());
    for expected in [
        "tool-one",
        "tool-two",
        "approval (tool)",
        "named Brain turn produced no wire source",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?}:\n{rendered}"
        );
        assert_eq!(
            rendered.matches(expected).count(),
            1,
            "duplicated {expected:?}"
        );
    }
    assert_eq!(rendered.matches("result").count(), 1);
}

#[test]
fn local_runner_projection_suppresses_matching_canonical_program_and_result() {
    let mut projection = LocalBrainProjection {
        run_id: crate::brain::store::RunId(uuid::Uuid::nil()),
        source: "(say \"hello\")".into(),
        output: "hello".into(),
        tool_ids: std::collections::HashSet::new(),
        approval_ids: std::collections::HashSet::new(),
        program_seq: None,
        transient_output_unit: None,
        failed: false,
    };
    let mut program = brain_event(
        12,
        "provider",
        crate::brain::store::BrainEventKind::Program {
            language: crate::brain::store::ProgramLanguage::Lisp,
            source: "(say \"hello\")".into(),
        },
    );
    program.run_id = Some(projection.run_id);
    assert_eq!(projection.observe(&program), LocalProjectionMatch::Suppress);
    assert_eq!(projection.program_seq, Some(12));

    let mut result = brain_event(
        14,
        "daemon",
        crate::brain::store::BrainEventKind::Result {
            request_seq: 12,
            output: "hello".into(),
            error: None,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
    );
    result.run_id = Some(projection.run_id);
    assert_eq!(
        projection.observe(&result),
        LocalProjectionMatch::SuppressAndComplete
    );
}

#[test]
fn local_runner_projection_does_not_hide_different_canonical_output() {
    let mut projection = LocalBrainProjection {
        run_id: crate::brain::store::RunId(uuid::Uuid::nil()),
        source: "(say \"hello\")".into(),
        output: "hello".into(),
        tool_ids: std::collections::HashSet::new(),
        approval_ids: std::collections::HashSet::new(),
        program_seq: Some(12),
        transient_output_unit: None,
        failed: false,
    };
    let mut result = brain_event(
        14,
        "daemon",
        crate::brain::store::BrainEventKind::Result {
            request_seq: 12,
            output: "different".into(),
            error: None,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
    );
    result.run_id = Some(projection.run_id);
    assert_eq!(projection.observe(&result), LocalProjectionMatch::None);
}

#[test]
fn participant_subject_is_not_the_brain_name() {
    assert_eq!(
        participant_subject_from("shammah", "workstation.local"),
        "shammah@workstation.local"
    );
}

#[test]
fn participant_subject_is_printable_and_bounded() {
    let subject = participant_subject_from(&format!("user\n{}", "x".repeat(200)), "");
    assert!(!subject.chars().any(char::is_control));
    assert!(subject.chars().count() <= 128);
    assert!(subject.ends_with('x'));
}

#[test]
fn runner_subject_identifies_one_frontend_not_only_its_participant() {
    let participant = "shammah@workstation.local";
    let first = runner_subject_from(
        participant,
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
    );
    let second = runner_subject_from(
        participant,
        Uuid::parse_str("11111111-0000-0000-0000-000000000001").unwrap(),
    );
    assert_ne!(first, second);
    assert!(first.starts_with(participant));
    assert!(first.chars().count() <= 128);
}

#[test]
fn local_participant_display_omits_only_the_matching_machine() {
    assert_eq!(
        participant_display_name(
            "shammah@Shammahs-MacBook-Air.local",
            Some("Shammahs-MacBook-Air.local")
        ),
        "shammah"
    );
    assert_eq!(
        participant_display_name(
            "shammah@Shammahs-MacBook-Air.local/frontend-12345678",
            Some("Shammahs-MacBook-Air.local")
        ),
        "shammah/frontend-12345678"
    );
    assert_eq!(
        participant_display_name("alice@remote.example", Some("local.example")),
        "alice@remote.example"
    );
    assert_eq!(
        participant_display_name("alice@remote.example", None),
        "alice@remote.example"
    );
}

#[test]
fn explicit_finch_address_strips_only_the_addressee() {
    assert_eq!(
        finch_addressed_prompt("  @finch   investigate this?!  "),
        Some("investigate this?!")
    );
    assert_eq!(finch_addressed_prompt("@finchbot hello"), None);
    assert_eq!(finch_addressed_prompt("@finch"), None);
    assert_eq!(finch_addressed_prompt("ordinary prompt"), None);
}
use crate::cli::repl_event::query_processor::apply_sliding_window;
// format_elapsed and format_token_count moved to tool_display; import for status-bar tests.
use crate::cli::repl_event::tool_display::{format_elapsed, format_token_count};

// Pulsing animation frames used in status-bar tests.
const THROB_FRAMES: &[&str] = &["✦", "✳", "✼", "✳"];

fn claude_profile(name: &str, model: &str) -> crate::config::ProviderEntry {
    crate::config::ProviderEntry::Claude {
        api_key: "test-key".to_string(),
        model: Some(model.to_string()),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some(name.to_string()),
    }
}

#[test]
fn test_model_profile_resolution_distinguishes_same_provider_models() {
    let profiles = vec![
        claude_profile("fast", "claude-haiku"),
        claude_profile("deep", "claude-opus"),
    ];
    assert_eq!(resolve_provider_profile(&profiles, "fast"), Ok(0));
    assert_eq!(resolve_provider_profile(&profiles, "deep"), Ok(1));
    assert_eq!(resolve_provider_profile(&profiles, "2"), Ok(1));
    assert!(resolve_provider_profile(&profiles, "claude")
        .unwrap_err()
        .contains("multiple profiles"));
}

#[test]
fn test_duplicate_model_profile_names_are_rejected_as_ambiguous() {
    let profiles = vec![
        claude_profile("work", "claude-haiku"),
        claude_profile("work", "claude-opus"),
    ];
    assert!(resolve_provider_profile(&profiles, "work")
        .unwrap_err()
        .contains("ambiguous"));
}

// --- streaming status bar format ---

#[test]
fn test_streaming_status_format() {
    // Verify the status bar message format used during streaming
    let verb = "Thinking"; // representative word; actual value comes from random_spinner_verb()
    let secs = 75u64;
    let tokens = 1600usize;
    let elapsed_str = format_elapsed(secs);
    let tokens_str = format_token_count(tokens);
    let icon = THROB_FRAMES[1]; // "✳"
    let status = format!(
        "{} {}… ({} · ↓ {} tokens)",
        icon, verb, elapsed_str, tokens_str
    );
    assert_eq!(status, "✳ Thinking… (1m 15s · ↓ 1.6k tokens)");
}

#[test]
fn test_streaming_status_format_short() {
    let verb = "Thinking";
    let secs = 9u64;
    let tokens = 42usize;
    let icon = THROB_FRAMES[0]; // "✦"
    let status = format!(
        "{} {}… ({} · ↓ {} tokens)",
        icon,
        verb,
        format_elapsed(secs),
        format_token_count(tokens)
    );
    assert_eq!(status, "✦ Thinking… (9s · ↓ 42 tokens)");
}

#[test]
fn test_streaming_status_thinking() {
    // While thinking (no text yet), status shows "· thinking" suffix
    let verb = "Thinking";
    let secs = 15u64;
    let icon = THROB_FRAMES[2]; // "✼"
    let status = format!("{} {}… ({} · thinking)", icon, verb, format_elapsed(secs));
    assert_eq!(status, "✼ Thinking… (15s · thinking)");
}

#[test]
fn test_streaming_status_with_input_tokens() {
    // With input token count available, show ↑ input · ↓ output
    let verb = "Thinking";
    let input_tokens: u32 = 1250;
    let output_tokens = 300usize;
    let secs = 10u64;
    let icon = THROB_FRAMES[1]; // "✳"
    let status = format!(
        "{} {}… ({} · ↑ {} · ↓ {} tokens)",
        icon,
        verb,
        format_elapsed(secs),
        format_token_count(input_tokens as usize),
        format_token_count(output_tokens),
    );
    assert_eq!(status, "✳ Thinking… (10s · ↑ 1.2k · ↓ 300 tokens)");
}

#[test]
fn test_streaming_status_thinking_with_input_tokens() {
    // Usage arrives before text — show ↑ input · thinking
    let verb = "Thinking";
    let input_tokens: u32 = 800;
    let secs = 3u64;
    let icon = THROB_FRAMES[0]; // "✦"
    let status = format!(
        "{} {}… ({} · ↑ {} · thinking)",
        icon,
        verb,
        format_elapsed(secs),
        format_token_count(input_tokens as usize),
    );
    assert_eq!(status, "✦ Thinking… (3s · ↑ 800 · thinking)");
}

#[test]
fn test_throb_frames_cycle() {
    // Frames cycle without panicking
    let mut idx = 0usize;
    for _ in 0..100 {
        idx = (idx + 1) % THROB_FRAMES.len();
        assert!(!THROB_FRAMES[idx].is_empty());
    }
    // After 4 steps we're back to frame 0
    assert_eq!(THROB_FRAMES.len(), 4);
}

// compact_tool_summary, tool_result_to_display, strip_ansi, bash_smart_summary
// tests moved to tool_display.rs (where those functions now live).

// ── PresentPlan display ───────────────────────────────────────────────────

#[test]
fn test_presentplan_label_shows_plan_title() {
    use crate::cli::repl_event::tool_display::format_tool_label;
    let label = format_tool_label(
        "PresentPlan",
        &serde_json::json!({"plan": "# Refactor Auth System\n\nDetails here..."}),
    );
    assert!(
        label.contains("Refactor Auth System"),
        "label should show plan title: {:?}",
        label
    );
    assert!(
        label.contains("PresentPlan"),
        "label should show tool name: {:?}",
        label
    );
}

#[test]
fn test_presentplan_label_fallback_when_no_heading() {
    use crate::cli::repl_event::tool_display::format_tool_label;
    let label = format_tool_label(
        "PresentPlan",
        &serde_json::json!({"plan": "Just some prose with no heading."}),
    );
    assert!(
        label.contains("proposing plan"),
        "should fall back to 'proposing plan': {:?}",
        label
    );
}

#[test]
fn test_presentplan_label_uses_first_heading_only() {
    use crate::cli::repl_event::tool_display::format_tool_label;
    let label = format_tool_label(
        "presentplan",
        &serde_json::json!({"plan": "# First Title\n## Second Title\n\nContent"}),
    );
    assert!(
        label.contains("First Title"),
        "should use first heading: {:?}",
        label
    );
    assert!(
        !label.contains("Second Title"),
        "should not show second heading: {:?}",
        label
    );
}

// --- find_last_exchange ---

fn user_msg(text: &str) -> crate::claude::Message {
    crate::claude::Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
    }
}

fn assistant_msg(text: &str) -> crate::claude::Message {
    crate::claude::Message {
        role: "assistant".to_string(),
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
    }
}

#[test]
fn find_last_exchange_empty_returns_empty_pair() {
    let (q, r) = find_last_exchange(&[]);
    assert!(q.is_empty());
    assert!(r.is_empty());
}

#[test]
fn find_last_exchange_only_user_messages() {
    let msgs = vec![user_msg("hello"), user_msg("world")];
    let (q, r) = find_last_exchange(&msgs);
    assert!(
        r.is_empty(),
        "no assistant msg → response should be empty: {:?}",
        r
    );
    assert!(q.is_empty());
}

#[test]
fn find_last_exchange_single_turn() {
    let msgs = vec![user_msg("What is 2+2?"), assistant_msg("4")];
    let (q, r) = find_last_exchange(&msgs);
    assert_eq!(q, "What is 2+2?");
    assert_eq!(r, "4");
}

#[test]
fn find_last_exchange_picks_latest_turn() {
    let msgs = vec![
        user_msg("First question"),
        assistant_msg("First answer"),
        user_msg("Second question"),
        assistant_msg("Second answer"),
    ];
    let (q, r) = find_last_exchange(&msgs);
    assert_eq!(q, "Second question");
    assert_eq!(r, "Second answer");
}

#[test]
fn find_last_exchange_skips_empty_assistant_text() {
    let msgs = vec![
        user_msg("Real question"),
        assistant_msg("Real answer"),
        user_msg("Ignored"),
        // Assistant message with empty text (e.g., tool-only response)
        crate::claude::Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: "   ".to_string(),
            }],
        },
    ];
    let (q, r) = find_last_exchange(&msgs);
    // Should skip the whitespace-only assistant msg and find the earlier real one
    assert_eq!(r, "Real answer");
    assert_eq!(q, "Real question");
}

#[test]
fn find_last_exchange_assistant_only_no_preceding_user() {
    let msgs = vec![assistant_msg("Unprompted response")];
    let (q, r) = find_last_exchange(&msgs);
    assert_eq!(r, "Unprompted response");
    // No user message precedes it
    assert!(q.is_empty(), "query should be empty: {:?}", q);
}

// --- apply_sliding_window ---

fn make_msgs(roles: &[&str]) -> Vec<crate::claude::Message> {
    roles
        .iter()
        .enumerate()
        .map(|(i, &role)| {
            let text = format!("msg {}", i);
            if role == "user" {
                user_msg(&text)
            } else {
                assistant_msg(&text)
            }
        })
        .collect()
}

#[test]
fn test_sliding_window_trims_to_max_verbatim() {
    // 30 alternating messages, max 20 → 20 returned, first is user
    let roles: Vec<&str> = (0..30)
        .map(|i| if i % 2 == 0 { "user" } else { "assistant" })
        .collect();
    let msgs = make_msgs(&roles);
    let result = apply_sliding_window(msgs, 20);
    assert_eq!(result.len(), 20);
    assert_eq!(result.first().unwrap().role, "user");
}

#[test]
fn test_sliding_window_disabled_when_zero() {
    let msgs = make_msgs(&["user", "assistant", "user", "assistant", "user"]);
    let len = msgs.len();
    let result = apply_sliding_window(msgs, 0);
    assert_eq!(result.len(), len);
}

#[test]
fn test_sliding_window_no_op_when_under_limit() {
    let msgs = make_msgs(&["user", "assistant", "user", "assistant"]);
    let result = apply_sliding_window(msgs, 20);
    assert_eq!(result.len(), 4);
    assert_eq!(result.first().unwrap().role, "user");
}

#[test]
fn test_sliding_window_skips_orphaned_assistant_at_boundary() {
    // 5 messages: u a u a u, window=3 → last 3 are [a, u, a] (index 2,3,4)
    // Leading 'a' gets skipped → result is [u, a] starting at index 3
    let msgs = make_msgs(&["user", "assistant", "user", "assistant", "user"]);
    // Swap last 3 to [assistant, user, assistant] by building manually:
    let roles = ["user", "assistant", "user", "assistant", "user"];
    // With window=3: last 3 = msgs[2..] = [user, assistant, user] → starts with user already
    // To actually trigger the skip, build a window that starts with assistant:
    let msgs2 = make_msgs(&["user", "assistant", "assistant", "user", "assistant"]);
    // window=3 → last 3 = [assistant(idx2), user(idx3), assistant(idx4)]
    // leading assistant removed → [user, assistant]
    let result = apply_sliding_window(msgs2, 3);
    assert_eq!(result.first().unwrap().role, "user");
    assert!(result.len() < 3); // shortened due to skipping
    let _ = roles; // silence unused warning
    let _ = msgs;
}

#[test]
fn test_sliding_window_minimum_guard_prevents_empty() {
    // All messages are assistant-role (pathological case)
    let msgs = make_msgs(&["assistant", "assistant", "assistant", "assistant"]);
    // window=3 → last 3 are all assistant; floor at 2 prevents empty
    let result = apply_sliding_window(msgs, 3);
    assert!(
        result.len() >= 2,
        "floor of 2 must be maintained; got {}",
        result.len()
    );
}

/// Regression: orphaned tool_result at window boundary must be stripped.
///
/// Scenario: conversation has two full tool-call round-trips followed by a
/// user text turn.  With a small window, the first round-trip's tool_use is
/// cut but its tool_result survives as the first message in the window.
/// All providers reject `tool_result` blocks without a matching `tool_use`.
#[test]
fn test_sliding_window_strips_orphaned_tool_result_at_boundary() {
    use crate::claude::Message;

    // Build:
    //   [0] user "question"          ← will be dropped by window
    //   [1] assistant with ToolUse   ← will be dropped by window (cut here)
    //   [2] user with ToolResult     ← ORPHANED — tool_use was dropped
    //   [3] assistant "answer 1"
    //   [4] user "next question"
    //   [5] assistant "answer 2"
    let tool_use_id = "call_orphan_test".to_string();

    let msgs: Vec<Message> = vec![
        // [0] old user turn (outside window)
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "question".to_string(),
            }],
        },
        // [1] assistant with ToolUse (will be cut by window)
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: tool_use_id.clone(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
            }],
        },
        // [2] user with ToolResult — orphaned when [1] is cut
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: "file1.rs\nfile2.rs".to_string(),
                is_error: None,
            }],
        },
        // [3] assistant reply
        assistant_msg("answer 1"),
        // [4] next user turn
        user_msg("next question"),
        // [5] assistant reply
        assistant_msg("answer 2"),
    ];

    // window=4 keeps msgs[2..] = [orphaned ToolResult user, assistant, user, assistant]
    let result = apply_sliding_window(msgs, 4);

    // The orphaned tool_result user turn ([2]) and its assistant response ([3])
    // must have been stripped, leaving [user "next question", assistant "answer 2"].
    assert!(
        result.len() >= 2,
        "must have at least 2 messages; got {}",
        result.len()
    );
    assert_eq!(
        result.first().unwrap().role,
        "user",
        "window must start with a user message"
    );
    // Crucially: the first user message must NOT be a tool_result-only message.
    let first_has_only_tool_results = result.first().map(|m| {
        m.content
            .iter()
            .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
    });
    assert_ne!(
        first_has_only_tool_results,
        Some(true),
        "orphaned tool_result user message must have been stripped"
    );
}

/// Regression: when ALL messages in the window are tool round-trips (no plain
/// user text), the old cascade-removal code would strip the orphaned tool_result
/// AND the next assistant, making the following tool_result an orphan, and so on
/// until the 2-message floor left a single orphaned tool_result at position 0.
/// The fix inserts a placeholder user turn instead of cascading.
#[test]
fn test_sliding_window_all_tool_rounds_no_cascade_orphan() {
    use crate::claude::Message;

    // Build a conversation that is ENTIRELY tool round-trips:
    //   [0] user "query"               ← outside window (dropped by slice)
    //   [1] asst tool_use(A)           ← outside window
    //   [2] user tool_result(A)        ← window start → ORPHANED
    //   [3] asst tool_use(B)           ← valid pair start
    //   [4] user tool_result(B)        ← valid
    //   [5] asst tool_use(C)
    //   [6] user tool_result(C)
    //
    // window=5 keeps msgs[2..] = [orphan, asst(B), user(B), asst(C), user(C)]
    let make_tool_result_msg = |id: &str| Message {
        role: "user".to_string(),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: id.to_string(),
            content: "ok".to_string(),
            is_error: None,
        }],
    };
    let make_tool_use_msg = |id: &str| Message {
        role: "assistant".to_string(),
        content: vec![ContentBlock::ToolUse {
            id: id.to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({}),
        }],
    };

    let msgs: Vec<Message> = vec![
        user_msg("query"),         // [0] outside window
        make_tool_use_msg("A"),    // [1] outside window
        make_tool_result_msg("A"), // [2] orphaned boundary
        make_tool_use_msg("B"),    // [3] valid
        make_tool_result_msg("B"), // [4] valid
        make_tool_use_msg("C"),    // [5] valid
        make_tool_result_msg("C"), // [6] valid
    ];

    let result = apply_sliding_window(msgs, 5);

    // Must start with a user message.
    assert_eq!(
        result.first().unwrap().role,
        "user",
        "window must start with user"
    );
    // The first user message must NOT be a pure tool_result (no orphan).
    let first_is_tool_result_only = result.first().map(|m| {
        m.content
            .iter()
            .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
    });
    assert_ne!(
        first_is_tool_result_only,
        Some(true),
        "orphaned tool_result must not be first: {:?}",
        result.first()
    );
    assert_eq!(
        result.first().unwrap().content[0].as_text(),
        Some("query"),
        "the dropped human request, not a synthetic placeholder, anchors retained tools"
    );
    // Valid tool rounds (B and C) must be preserved.
    assert!(
        result.len() >= 4,
        "valid tool round-trips B and C should be in window; got {} messages",
        result.len()
    );
}

/// Regression: orphaned tool_use (assistant sent tool_use but query was
/// cancelled before tool_result was added). The final-pass validator in
/// apply_sliding_window should strip the orphaned tool_use blocks, keeping
/// any text content, so the conversation sent to the provider is clean.
#[test]
fn test_sliding_window_strips_orphaned_tool_use() {
    use crate::claude::Message;

    // Simulate a cancelled query: assistant wrote tool_uses but the
    // corresponding tool_result user message was never added.
    //   [0] user "query"
    //   [1] assistant: text + tool_use A  ← orphaned (no tool_result follows)
    //   [2] user "fix it"
    let msgs: Vec<Message> = vec![
        user_msg("query"),
        Message {
            role: "assistant".to_string(),
            content: vec![
                ContentBlock::Text {
                    text: "I'll analyze that.".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "toolu_orphan".to_string(),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path": "/foo"}),
                },
            ],
        },
        user_msg("fix it"),
    ];

    let result = apply_sliding_window(msgs, 20);

    // The orphaned tool_use block must be stripped; text content kept.
    for msg in &result {
        let has_orphaned_tool_use = msg.content.iter().any(|b| {
            if let ContentBlock::ToolUse { id, .. } = b {
                id == "toolu_orphan"
            } else {
                false
            }
        });
        assert!(
            !has_orphaned_tool_use,
            "orphaned tool_use must be stripped; got message: {:?}",
            msg
        );
    }

    // The text content ("I'll analyze that.") should be preserved.
    let has_text = result.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("I'll analyze")))
    });
    assert!(
        has_text,
        "text content of orphaned assistant message must be preserved"
    );

    // Window must start with a user message.
    assert_eq!(result.first().unwrap().role, "user");

    // "fix it" must still be present.
    let has_fix_it = result.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("fix it")))
    });
    assert!(has_fix_it, "user follow-up message must be preserved");
}

// ── tool_approval_summary ────────────────────────────────────────────────

fn make_tool_use(name: &str, input: serde_json::Value) -> crate::tools::types::ToolUse {
    crate::tools::types::ToolUse {
        id: "test_id".to_string(),
        name: name.to_string(),
        input,
    }
}

#[test]
fn test_tool_approval_summary_bash_with_command() {
    let tool = make_tool_use(
        "bash",
        serde_json::json!({"command": "git push origin main"}),
    );
    assert_eq!(
        tool_approval_summary(&tool),
        "Command: git push origin main"
    );
}

#[test]
fn test_tool_approval_summary_bash_uppercase() {
    let tool = make_tool_use("Bash", serde_json::json!({"command": "cargo test"}));
    assert_eq!(tool_approval_summary(&tool), "Command: cargo test");
}

#[test]
fn test_tool_approval_summary_bash_long_command_truncated() {
    let long_cmd = "a".repeat(70);
    let tool = make_tool_use("bash", serde_json::json!({"command": long_cmd}));
    let result = tool_approval_summary(&tool);
    assert!(
        result.starts_with("Command: "),
        "should start with 'Command: ': {}",
        result
    );
    assert!(
        result.contains("..."),
        "long command should be truncated with '...': {}",
        result
    );
}

#[test]
fn test_tool_approval_summary_bash_no_command() {
    let tool = make_tool_use("bash", serde_json::json!({}));
    assert_eq!(tool_approval_summary(&tool), "Execute shell command");
}

#[test]
fn test_tool_approval_summary_read_with_path() {
    let tool = make_tool_use("read", serde_json::json!({"file_path": "src/main.rs"}));
    assert_eq!(tool_approval_summary(&tool), "File: src/main.rs");
}

#[test]
fn test_tool_approval_summary_read_uppercase() {
    let tool = make_tool_use("Read", serde_json::json!({"file_path": "/a/b/c.rs"}));
    assert_eq!(tool_approval_summary(&tool), "File: /a/b/c.rs");
}

#[test]
fn test_tool_approval_summary_read_no_path() {
    let tool = make_tool_use("read", serde_json::json!({}));
    assert_eq!(tool_approval_summary(&tool), "Read file");
}

#[test]
fn test_tool_approval_summary_grep_with_pattern() {
    let tool = make_tool_use(
        "grep",
        serde_json::json!({"pattern": "fn main", "path": "src"}),
    );
    assert_eq!(tool_approval_summary(&tool), "Pattern: fn main");
}

#[test]
fn test_tool_approval_summary_grep_long_pattern_truncated() {
    let long = "x".repeat(50);
    let tool = make_tool_use("grep", serde_json::json!({"pattern": long}));
    let result = tool_approval_summary(&tool);
    assert!(result.starts_with("Pattern: "), "got: {}", result);
    assert!(
        result.contains("..."),
        "long pattern should truncate: {}",
        result
    );
}

#[test]
fn test_tool_approval_summary_grep_no_pattern() {
    let tool = make_tool_use("Grep", serde_json::json!({}));
    assert_eq!(tool_approval_summary(&tool), "Search files");
}

#[test]
fn test_tool_approval_summary_glob_with_pattern() {
    let tool = make_tool_use("glob", serde_json::json!({"pattern": "**/*.rs"}));
    assert_eq!(tool_approval_summary(&tool), "Pattern: **/*.rs");
}

#[test]
fn test_tool_approval_summary_glob_uppercase_no_pattern() {
    let tool = make_tool_use("Glob", serde_json::json!({}));
    assert_eq!(tool_approval_summary(&tool), "Find files");
}

#[test]
fn test_tool_approval_summary_enter_plan_mode_with_reason() {
    let tool = make_tool_use(
        "EnterPlanMode",
        serde_json::json!({"reason": "Need to research the codebase"}),
    );
    assert_eq!(
        tool_approval_summary(&tool),
        "Reason: Need to research the codebase"
    );
}

#[test]
fn test_tool_approval_summary_enter_plan_mode_long_reason_truncated() {
    let long_reason = "r".repeat(60);
    let tool = make_tool_use("EnterPlanMode", serde_json::json!({"reason": long_reason}));
    let result = tool_approval_summary(&tool);
    assert!(result.starts_with("Reason: "), "got: {}", result);
    assert!(
        result.contains("..."),
        "long reason should truncate: {}",
        result
    );
}

#[test]
fn test_tool_approval_summary_enter_plan_mode_no_reason() {
    let tool = make_tool_use("EnterPlanMode", serde_json::json!({}));
    assert_eq!(tool_approval_summary(&tool), "Enter planning mode");
}

#[test]
fn test_tool_approval_summary_unknown_tool() {
    let tool = make_tool_use("WebFetch", serde_json::json!({"url": "https://docs.rs"}));
    assert_eq!(tool_approval_summary(&tool), "Execute WebFetch tool");
}

// ── dialog_result_to_confirmation (3-option Claude Code style) ───────────

#[test]
fn test_dialog_result_selected_0_approve_once() {
    // Option "1. Yes" → ApproveOnce
    let tool = make_tool_use("bash", serde_json::json!({"command": "ls"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(0), &tool);
    assert!(
        matches!(
            result,
            crate::cli::repl_event::events::ConfirmationResult::ApproveOnce
        ),
        "index 0 (Yes) should be ApproveOnce, got {:?}",
        result
    );
}

#[test]
fn test_dialog_result_selected_1_approve_pattern_session() {
    // Option "2. Yes, and don't ask again for: bash:*" → ApprovePatternSession
    let tool = make_tool_use("bash", serde_json::json!({"command": "git status"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(1), &tool);
    match result {
        crate::cli::repl_event::events::ConfirmationResult::ApprovePatternSession(p) => {
            assert_eq!(p.tool_name, "bash");
            assert_eq!(p.pattern, "*");
            assert!(
                p.description.contains("session"),
                "description: {}",
                p.description
            );
        }
        other => panic!("expected ApprovePatternSession, got {:?}", other),
    }
}

#[test]
fn test_dialog_result_selected_2_deny() {
    // Option "3. No" → Deny
    let tool = make_tool_use("bash", serde_json::json!({"command": "rm -rf /"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(2), &tool);
    assert!(
        matches!(
            result,
            crate::cli::repl_event::events::ConfirmationResult::Deny
        ),
        "index 2 (No) should be Deny, got {:?}",
        result
    );
}

#[test]
fn test_dialog_result_selected_high_index_deny() {
    let tool = make_tool_use("bash", serde_json::json!({"command": "echo hi"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(99), &tool);
    assert!(
        matches!(
            result,
            crate::cli::repl_event::events::ConfirmationResult::Deny
        ),
        "out-of-range index should be Deny, got {:?}",
        result
    );
}

#[test]
fn test_dialog_result_cancelled_deny() {
    let tool = make_tool_use("bash", serde_json::json!({"command": "echo hi"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Cancelled, &tool);
    assert!(
        matches!(
            result,
            crate::cli::repl_event::events::ConfirmationResult::Deny
        ),
        "Cancelled should be Deny, got {:?}",
        result
    );
}

#[test]
fn test_dialog_result_custom_text_deny() {
    let tool = make_tool_use("bash", serde_json::json!({"command": "ls"}));
    let result = dialog_result_to_confirmation(
        crate::cli::tui::DialogResult::CustomText("please allow".to_string()),
        &tool,
    );
    assert!(
        matches!(
            result,
            crate::cli::repl_event::events::ConfirmationResult::Deny
        ),
        "CustomText should be Deny (safety), got {:?}",
        result
    );
}

#[test]
fn test_dialog_result_pattern_session_uses_tool_name() {
    // Verify the "don't ask again" pattern uses the actual tool name
    let tool = make_tool_use("grep", serde_json::json!({"pattern": "TODO"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(1), &tool);
    match result {
        crate::cli::repl_event::events::ConfirmationResult::ApprovePatternSession(p) => {
            assert_eq!(
                p.tool_name, "grep",
                "pattern tool_name should match tool: {}",
                p.tool_name
            );
        }
        other => panic!("expected ApprovePatternSession, got {:?}", other),
    }
}

#[test]
fn test_pattern_session_tool_name_matches_tool_use() {
    // The pattern's tool_name must match the tool being approved —
    // otherwise the cache won't recognise future calls to the same tool.
    // Index 1 = "2. Yes, and don't ask again for: Bash:*"
    let tool = make_tool_use("Bash", serde_json::json!({"command": "cargo fmt"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(1), &tool);
    match result {
        crate::cli::repl_event::events::ConfirmationResult::ApprovePatternSession(p) => {
            assert_eq!(
                p.tool_name, "Bash",
                "pattern tool_name should match ToolUse.name"
            );
        }
        other => panic!("expected ApprovePatternSession, got {:?}", other),
    }
}

#[test]
fn test_pattern_persistent_tool_name_matches_tool_use() {
    // Persistent approval is no longer in the 3-option dialog.
    // Index 2 → Deny; index 99 → Deny. Just verify nothing panics.
    let tool = make_tool_use("read", serde_json::json!({"file_path": "src/lib.rs"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(2), &tool);
    assert!(
        matches!(
            result,
            crate::cli::repl_event::events::ConfirmationResult::Deny
        ),
        "index 2 is No/Deny in 3-option dialog, got {:?}",
        result
    );
}
