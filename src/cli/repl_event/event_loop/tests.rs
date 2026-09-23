struct NeverCompletes;

#[async_trait::async_trait]
impl crate::generators::Generator for NeverCompletes {
    async fn generate(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<crate::generators::GeneratorResponse> {
        std::future::pending().await
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
                max_context_messages: Some(10),
            };
        &CAPABILITIES
    }

    fn name(&self) -> &str {
        "never-completes"
    }
}

#[tokio::test]
async fn agent_lifecycle_lag_resnapshots_authoritative_active_tasks_before_new_events() {
    use std::sync::Arc;

    let scheduler = crate::scheduler::AgentScheduler::new(
        crate::scheduler::ProviderResolver::new(Arc::new(NeverCompletes)),
        Arc::new(crate::runtime::ProgramRuntime::new()),
    );
    // Subscribe before overflowing the scheduler's 256-event broadcast ring.
    let events = scheduler.subscribe();
    let mut identities = Vec::new();
    for index in 0..260 {
        identities.push(
            scheduler
                .spawn(
                    crate::scheduler::AgentTaskSpec {
                        task: format!("lag child {index}"),
                        role: crate::scheduler::AgentRole::Explore,
                        background: None,
                        provider: None,
                        model: None,
                        context: Vec::new(),
                        capability_grant_ids: None,
                        budget: crate::scheduler::AgentBudget::default(),
                    },
                    None,
                )
                .await
                .expect("lag fixture child must spawn"),
        );
    }
    let completed = identities.remove(0);
    scheduler
        .cancel(completed.task_id)
        .await
        .expect("the stale terminal fixture must accept cancellation");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        scheduler.wait(completed.task_id),
    )
    .await
    .expect("the terminal event used to force stale state must settle")
    .expect("the cancelled fixture must retain its terminal result");

    let (target_tx, mut target_rx) = tokio::sync::mpsc::unbounded_channel();
    let bridge = tokio::spawn(super::forward_agent_events(
        Arc::clone(&scheduler),
        events,
        target_tx,
    ));
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), target_rx.recv())
        .await
        .expect("lag recovery must not stall")
        .expect("lag recovery channel must remain open");
    let super::ReplEvent::AgentLifecycle(crate::scheduler::AgentEvent::Resnapshot { active }) =
        first
    else {
        panic!("lag must resnapshot before presenting retained transitions as current; first_event={first:?}");
    };
    assert_eq!(active.len(), identities.len(), "authoritative recovery must contain every still-active child; active_count={} spawned_count={}", active.len(), identities.len());
    assert!(active.iter().all(|entry| entry.task.identity.task_id != completed.task_id), "a child whose terminal event may have been lost must not survive the authoritative snapshot; completed_task={} active={active:?}", completed.task_id);

    bridge.abort();
    for identity in &identities {
        scheduler
            .cancel(identity.task_id)
            .await
            .expect("every active lag fixture must accept cleanup cancellation");
    }
    for identity in identities {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            scheduler.wait(identity.task_id),
        )
        .await
        .expect("cancelled lag fixture must settle")
        .expect("cancelled lag fixture must retain its terminal result");
    }
}

fn lifecycle_snapshot(
    runtime: &crate::runtime::ProgramRuntime,
    task_id: uuid::Uuid,
    status: crate::scheduler::AgentTaskStatus,
) -> crate::scheduler::AgentTaskSnapshot {
    crate::scheduler::AgentTaskSnapshot {
        identity: crate::scheduler::AgentIdentity {
            agent_id: uuid::Uuid::new_v4(),
            task_id,
            parent_agent_id: None,
            root_agent_id: uuid::Uuid::new_v4(),
            depth: 0,
            provider_model: "test-provider".into(),
            vm_revision: runtime.revision(),
            manifest_generation: runtime.manifest_generation(),
            starting_context_hash: "test-context".into(),
            grant_ceiling: crate::vm::EffectSet::pure(),
            brain_run_id: None,
        },
        task: format!("child {task_id}"),
        role: crate::scheduler::AgentRole::Explore,
        status,
        result: None,
    }
}

#[tokio::test]
async fn test_boundary_01_real_event_loop_dispatch_keeps_usage_refreshes_out_of_scrollback() {
    tokio::task::LocalSet::new()
        .run_until(boundary_01_dispatch_scenario())
        .await;
}

#[tokio::test]
async fn home_watch_failure_clears_todo_journal_target() {
    tokio::task::LocalSet::new()
        .run_until(home_watch_failure_clears_todo_journal_target_scenario())
        .await;
}

async fn home_watch_failure_clears_todo_journal_target_scenario() {
    use std::sync::Arc;
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    let tempdir = tempfile::tempdir().expect("create isolated tool state");
    let executor = crate::tools::ToolExecutor::new(
        crate::tools::ToolRegistry::new(),
        crate::tools::PermissionManager::new(),
        tempdir.path().join("patterns.json"),
    )
    .expect("construct inert tool executor");
    let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
    let mut event_loop = super::EventLoop::new_named_brain_test_runner(
        generator,
        Vec::new(),
        Arc::new(tokio::sync::Mutex::new(executor)),
        Arc::clone(&runtime),
    );
    event_loop
        .handle_event(super::ReplEvent::HomeBrainWatchFailed {
            epoch: 0,
            error: Some("Disconnected: Peer disconnected.".into()),
        })
        .await
        .expect("watch failure dispatch must succeed");
    assert!(
        !event_loop.todo_journal_is_bound_for_test(),
        "todo_write must not keep a clone of the disconnected home Brain client"
    );
}

async fn boundary_01_dispatch_scenario() {
    use std::sync::Arc;

    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    let tempdir = tempfile::tempdir().expect("TEST-BOUNDARY-01: create isolated tool state");
    let executor = crate::tools::ToolExecutor::new(
        crate::tools::ToolRegistry::new(),
        crate::tools::PermissionManager::new(),
        tempdir.path().join("patterns.json"),
    )
    .expect("TEST-BOUNDARY-01: construct inert tool executor");
    let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
    let mut event_loop = super::EventLoop::new_named_brain_test_runner(
        generator,
        Vec::new(),
        Arc::new(tokio::sync::Mutex::new(executor)),
        Arc::clone(&runtime),
    );
    let first_id = uuid::Uuid::new_v4();
    event_loop
        .handle_event(super::ReplEvent::AgentLifecycle(
            crate::scheduler::AgentEvent::TaskQueued {
                snapshot: lifecycle_snapshot(
                    &runtime,
                    first_id,
                    crate::scheduler::AgentTaskStatus::Queued,
                ),
            },
        ))
        .await
        .expect("TEST-BOUNDARY-01: queued lifecycle dispatch must succeed");
    event_loop
        .handle_event(super::ReplEvent::AgentLifecycle(
            crate::scheduler::AgentEvent::UsageUpdated {
                task_id: first_id,
                usage: crate::scheduler::AgentUsage {
                    state: crate::scheduler::AgentUsageState::Complete,
                    input_tokens: Some(12),
                    output_tokens: Some(7),
                    reported_attempts: 1,
                    started_attempts: 1,
                },
            },
        ))
        .await
        .expect("TEST-BOUNDARY-01: usage lifecycle dispatch must succeed");
    assert!(event_loop.output_manager.get_messages().is_empty(), "TEST-BOUNDARY-01: queued and usage refreshes must update only live projection, not scrollback; message_count={}", event_loop.output_manager.get_messages().len());
    let usage_line = event_loop
        .status_bar
        .get_line(&crate::cli::status_bar::StatusLineType::AgentActivity)
        .expect("TEST-BOUNDARY-01: usage dispatch must reach the status projection");
    assert!(
        usage_line.contains("12 input, 7 output")
            && usage_line.contains("complete (1/1 attempts reported)"),
        "TEST-BOUNDARY-01: real dispatch must render truthful usage text; line={usage_line:?}"
    );

    let second_id = uuid::Uuid::new_v4();
    let second = lifecycle_snapshot(
        &runtime,
        second_id,
        crate::scheduler::AgentTaskStatus::Running,
    );
    event_loop
        .handle_event(super::ReplEvent::AgentLifecycle(
            crate::scheduler::AgentEvent::Resnapshot {
                active: vec![crate::scheduler::AgentActivitySnapshot {
                    task: second.clone(),
                    usage: crate::scheduler::AgentUsage::default(),
                    active_tool: None,
                }],
            },
        ))
        .await
        .expect("TEST-BOUNDARY-01: authoritative resnapshot dispatch must succeed");
    assert!(event_loop.output_manager.get_messages().is_empty(), "TEST-BOUNDARY-01: lag recovery must replace live state without appending scrollback; message_count={}", event_loop.output_manager.get_messages().len());
    let resnapshot_line = event_loop
        .status_bar
        .get_line(&crate::cli::status_bar::StatusLineType::AgentActivity)
        .expect("TEST-BOUNDARY-01: resnapshot must retain one current child");
    assert!(resnapshot_line.contains("Children: 1 active") && resnapshot_line.contains("unavailable input, unavailable output"), "TEST-BOUNDARY-01: resnapshot must replace stale usage with authoritative state; line={resnapshot_line:?}");

    let result = crate::scheduler::AgentTaskResult {
        identity: second.identity,
        status: crate::scheduler::AgentTaskStatus::Completed,
        final_message: "done".into(),
        diagnostics: Vec::new(),
        turns: 1,
        elapsed_ms: 1,
    };
    event_loop
        .handle_event(super::ReplEvent::AgentLifecycle(
            crate::scheduler::AgentEvent::TaskFinished {
                result,
                usage: crate::scheduler::AgentUsage {
                    state: crate::scheduler::AgentUsageState::Complete,
                    input_tokens: Some(3),
                    output_tokens: Some(2),
                    reported_attempts: 1,
                    started_attempts: 1,
                },
            },
        ))
        .await
        .expect("TEST-BOUNDARY-01: terminal lifecycle dispatch must succeed");
    let messages = event_loop.output_manager.get_messages();
    assert_eq!(messages.len(), 1, "TEST-BOUNDARY-01: one unbound child terminal must append one structured activity unit; message_count={} messages={:?}", messages.len(), messages.iter().map(|message| message.format(&crate::theme::ColorScheme::default())).collect::<Vec<_>>());
    assert_eq!(
        crate::cli::test_projection::try_project_for_test(
            messages[0].as_ref(),
            &crate::theme::ColorScheme::default()
        )
        .expect("projected row")
        .role,
        crate::cli::test_projection::NodeRole::Activity,
        "TEST-BOUNDARY-01: the terminal child summary must not regress to a loose information line"
    );
    assert_eq!(
        event_loop
            .status_bar
            .get_line(&crate::cli::status_bar::StatusLineType::AgentActivity),
        None,
        "TEST-BOUNDARY-01: terminal projection must clean up the final active status cell"
    );
}

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
    use crate::cli::conversation::ToolRoundProgress;
    use crate::providers::{ContentBlock, Message};

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
    assert!(super::commit_tool_round_and_continue(
        &conversation,
        query_id,
        token,
        &llm_tx,
        None,
        &[],
    )
    .await
    .is_err());
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
        &[],
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
    assert!(super::commit_tool_round_and_continue(
        &conversation,
        query_id,
        token,
        &llm_tx,
        None,
        &[],
    )
    .await
    .is_err());
    assert!(
        observed_rx.try_recv().is_err(),
        "completed round must admit only one continuation"
    );
}

#[tokio::test]
async fn closed_worker_leaves_complete_round_staged_and_provider_invisible() {
    use crate::providers::{ContentBlock, Message};
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
        super::commit_tool_round_and_continue(&conversation, query_id, token, &tx, None, &[]).await,
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
    use crate::providers::{ContentBlock, Message};
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
            &[],
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
async fn publication_failure_rolls_back_injected_pending_user_text() {
    use crate::providers::{ContentBlock, Message};
    let query_id = uuid::Uuid::new_v4();
    let mut history = crate::cli::conversation::ConversationHistory::new();
    history.add_user_message("original".to_string());
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
            let _ = spawned.unwrap().send(());
            drop(publication);
        }
    });

    assert_eq!(
        super::commit_tool_round_and_continue(
            &conversation,
            query_id,
            token,
            &tx,
            None,
            &["steer now".to_string()],
        )
        .await,
        Err(crate::cli::conversation::ToolRoundError::ContinuationUnavailable)
    );
    let live = conversation.read().await.get_messages();
    assert_eq!(
        live.len(),
        1,
        "publication failure must restore pre-inject history; live={live:?}"
    );
    assert_eq!(live[0].text_content(), "original");
    assert!(
        conversation
            .read()
            .await
            .completed_tool_results(query_id, token)
            .is_ok(),
        "the tool round must be staged again so a retry does not double-commit"
    );
}

#[tokio::test]
async fn checkpoint_failure_revokes_spawned_continuation_and_restores_live_history() {
    use crate::providers::{ContentBlock, Message};
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
            &[],
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
    let schedule_id = crate::brain::ScheduleId(uuid::Uuid::new_v4());
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
        assert_eq!(language, crate::brain::ProgramLanguage::Lisp);
        assert_eq!(source, "(say \"later\")");
        assert!(grant_ceiling.is_pure());
        assert_eq!(next_due_ms, 1_770_000_000_000);
        assert_eq!(interval_ms, None);
        assert_eq!(
            delivery_policy,
            crate::brain::BrainScheduleDeliveryPolicy::Coalesce
        );
        response_tx
            .send(Ok(crate::brain::BrainSchedule {
                schedule_id,
                initiating_attachment_id: crate::brain::AttachmentId(uuid::Uuid::new_v4()),
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
        crate::brain::ProgramLanguage::Lisp,
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
    let tool_use = crate::tools::ToolUse {
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
use crate::cli::messages::Message;

fn lifecycle_test_event_loop() -> (EventLoop, Arc<crate::cli::output_manager::OutputManager>) {
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    let patterns = tempfile::tempdir().unwrap().path().join("patterns.json");
    let executor = crate::tools::ToolExecutor::new(
        crate::tools::ToolRegistry::new(),
        crate::tools::PermissionManager::new(),
        patterns,
    )
    .unwrap();
    let event_loop = EventLoop::new_named_brain_test_runner(
        Arc::new(NeverCompletes),
        Vec::new(),
        Arc::new(tokio::sync::Mutex::new(executor)),
        Arc::clone(&runtime),
    );
    assert!(
        runtime.agent_binding_for_test(None).is_some(),
        "the named-Brain production-boundary runner must explicitly attach its scheduler"
    );
    let output = Arc::clone(&event_loop.output_manager);
    (event_loop, output)
}

#[tokio::test]
async fn test_named_brain_runner_attaches_its_scheduler() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (_event_loop, _output) = lifecycle_test_event_loop();
        })
        .await;
}

/// Production-boundary regression for the loop-detected TUI dump: a blocked
/// bash result must update the labeled bash row, put the full diagnostic in
/// the expandable body, and must not spawn a second WorkUnit titled with the
/// raw provider tool id (`call_tyIrmyNxiUxYGF7QhOT1vslZ`).
#[tokio::test]
async fn test_loop_detected_tool_result_updates_labeled_row_not_raw_id_fallback() {
    tokio::task::LocalSet::new()
        .run_until(async {

            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let query_id = event_loop.query_states.create_query(Vec::new()).await;
            let tool_id = "call_tyIrmyNxiUxYGF7QhOT1vslZ".to_string();
            let command = serde_json::json!({"command": "git status --porcelain"});
            let round_token = event_loop
                .conversation
                .write()
                .await
                .stage_assistant(
                    query_id,
                    crate::providers::Message {
                        role: "assistant".to_string(),
                        content: vec![crate::providers::ContentBlock::ToolUse {
                            id: tool_id.clone(),
                            name: "bash".to_string(),
                            input: command.clone(),
                        }],
                    },
                )
                .expect("stage the tool round handle_tool_result records into");

            let work_unit = output.start_work_unit("Tools");
            event_loop
                .query_states
                .set_tool_work_unit(query_id, Some(Arc::clone(&work_unit)))
                .await;
            let row_idx = work_unit.add_row(
                crate::cli::repl_event::tool_display::format_tool_label("bash", &command),
            );
            event_loop.active_tool_uses.write().await.insert(
                tool_id.clone(),
                (
                    "bash".to_string(),
                    command,
                    Arc::clone(&work_unit),
                    row_idx,
                ),
            );

            let error = anyhow::anyhow!(
                "loop detected: bash called 3 times with the same arguments and the same result\n\
                 Repeating this call produced no new information. \
                 Inspect the previous results or use a different command."
            );
            event_loop
                .handle_tool_result(query_id, round_token, tool_id.clone(), Err(error))
                .await
                .expect("loop ToolResult must apply to the registered bash row");

            let labels: Vec<String> = output
                .get_messages()
                .iter()
                .filter_map(|message| {
                    crate::cli::test_projection::try_project_for_test(message.as_ref(), &crate::theme::ColorScheme::default())
                        .map(|row| row.label)
                })
                .collect();
            assert_eq!(
                labels.len(),
                1,
                "handle_tool_result must not spawn a fallback WorkUnit titled with the raw tool id; labels={labels:?}"
            );
            assert!(
                labels[0].contains("bash"),
                "Tools header must keep the bash label; got {}",
                labels[0]
            );
            assert!(
                !labels[0].contains(&tool_id),
                "Tools header must not echo the raw provider tool id; got {}",
                labels[0]
            );
            assert!(
                labels[0].contains("loop detected"),
                "Tools header must name the loop; got {}",
                labels[0]
            );

            let projected = crate::cli::test_projection::try_project_for_test(work_unit.as_ref(), &crate::theme::ColorScheme::default()).expect("projected row");
            let call = &projected.children[0];
            assert!(
                call.label.contains("bash"),
                "child must stay a bash row, not {tool_id}; got {}",
                call.label
            );
            assert!(
                !call.label.contains(&tool_id),
                "child must not be titled with the raw provider tool id; got {}",
                call.label
            );
            assert!(
                call.label.contains("failed"),
                "labeled bash row must be Error; got {}",
                call.label
            );
            let output_row = call
                .children
                .iter()
                .find(|child| child.role == crate::cli::test_projection::NodeRole::ToolOutput)
                .unwrap_or_else(|| {
                    panic!("long loop diagnostic must be expandable output; call={call:?}")
                });
            assert!(
                output_row
                    .body
                    .iter()
                    .any(|line| line.contains("produced no new information")),
                "expanding the failed bash row must show the full loop diagnostic; output={output_row:?}"
            );
        })
        .await;
}

/// A loop-detected ToolResult that was never registered must still attach to
/// the query Tools unit instead of `start_work_unit("Tool")`.
#[tokio::test]
async fn test_untracked_tool_result_attaches_to_query_tools_unit() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let query_id = event_loop.query_states.create_query(Vec::new()).await;
            let tool_id = "call_itsGgV2WKoaKTE5SuU6nTg1N".to_string();
            let round_token = event_loop
                .conversation
                .write()
                .await
                .stage_assistant(
                    query_id,
                    crate::providers::Message {
                        role: "assistant".to_string(),
                        content: vec![crate::providers::ContentBlock::ToolUse {
                            id: tool_id.clone(),
                            name: "bash".to_string(),
                            input: serde_json::json!({"command": "git status"}),
                        }],
                    },
                )
                .expect("stage the untracked tool round");
            let work_unit = output.start_work_unit("Tools");
            work_unit.add_row(crate::cli::repl_event::tool_display::format_tool_label(
                "bash",
                &serde_json::json!({"command": "git status"}),
            ));
            event_loop
                .query_states
                .set_tool_work_unit(query_id, Some(Arc::clone(&work_unit)))
                .await;

            event_loop
                .handle_tool_result(
                    query_id,
                    round_token,
                    tool_id.clone(),
                    Err(anyhow::anyhow!("LOOP DETECTED: You have called bash")),
                )
                .await
                .expect("untracked ToolResult must attach to the query Tools unit");

            let labels: Vec<String> = output
                .get_messages()
                .iter()
                .filter_map(|message| {
                    crate::cli::test_projection::try_project_for_test(
                        message.as_ref(),
                        &crate::theme::ColorScheme::default(),
                    )
                    .map(|row| row.label)
                })
                .collect();
            assert_eq!(
                labels.len(),
                1,
                "untracked ToolResult must not start a second Tools root; labels={labels:?}"
            );
            assert!(
                !labels.iter().any(|label| label.contains(&tool_id)),
                "no root may be titled with the raw provider tool id; labels={labels:?}"
            );
        })
        .await;
}

fn lifecycle_identity(
    agent_id: uuid::Uuid,
    task_id: uuid::Uuid,
    parent_agent_id: Option<uuid::Uuid>,
    root_agent_id: uuid::Uuid,
    depth: usize,
) -> crate::scheduler::AgentIdentity {
    crate::scheduler::AgentIdentity {
        agent_id,
        task_id,
        parent_agent_id,
        root_agent_id,
        depth,
        provider_model: "test/model".into(),
        vm_revision: 1,
        manifest_generation: 1,
        starting_context_hash: "test-context".into(),
        grant_ceiling: crate::vm::EffectSet::default(),
        brain_run_id: None,
    }
}

fn lifecycle_task_snapshot(
    identity: crate::scheduler::AgentIdentity,
    task: &str,
    status: crate::scheduler::AgentTaskStatus,
) -> crate::scheduler::AgentTaskSnapshot {
    crate::scheduler::AgentTaskSnapshot {
        identity,
        task: task.into(),
        role: crate::scheduler::AgentRole::Code,
        status,
        result: None,
    }
}

fn lifecycle_task_finished(
    identity: crate::scheduler::AgentIdentity,
    status: crate::scheduler::AgentTaskStatus,
    message: &str,
) -> crate::scheduler::AgentEvent {
    crate::scheduler::AgentEvent::TaskFinished {
        result: crate::scheduler::AgentTaskResult {
            identity,
            status,
            final_message: message.into(),
            diagnostics: Vec::new(),
            turns: 2,
            elapsed_ms: 12,
        },
        usage: crate::scheduler::AgentUsage::default(),
    }
}

#[test]
fn test_issue_652_lifecycle_provider_spawn_stays_in_originating_work_unit() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(tokio::task::LocalSet::new().run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            let query_id = event_loop
                .query_states
                .create_query(Vec::new())
                .await;
            let tool_id = "spawn-1".to_string();
            let round_token = event_loop
                .conversation
                .write()
                .await
                .stage_assistant(
                    query_id,
                    crate::providers::Message {
                        role: "assistant".into(),
                        content: vec![crate::providers::ContentBlock::ToolUse {
                            id: tool_id.clone(),
                            name: "spawn_agent".into(),
                            input: serde_json::json!({"task": "inspect lifecycle grouping"}),
                        }],
                    },
                )
                .unwrap();
            let unit = output.start_work_unit("Tools");
            let row = unit.add_row("spawn_agent(inspect lifecycle grouping)");
            event_loop.active_tool_uses.write().await.insert(
                tool_id.clone(),
                (
                    "spawn_agent".into(),
                    serde_json::json!({"task": "inspect lifecycle grouping"}),
                    Arc::clone(&unit),
                    row,
                ),
            );

            let agent_id = uuid::Uuid::new_v4();
            let task_id = uuid::Uuid::new_v4();
            let identity = lifecycle_identity(agent_id, task_id, None, agent_id, 0);
            let snapshot = crate::scheduler::AgentTaskSnapshot {
                identity: identity.clone(),
                task: "inspect lifecycle grouping".into(),
                role: crate::scheduler::AgentRole::Code,
                status: crate::scheduler::AgentTaskStatus::Running,
                result: None,
            };
            let mut queued = snapshot.clone();
            queued.status = crate::scheduler::AgentTaskStatus::Queued;
            event_loop
                .handle_event(ReplEvent::AgentLifecycle(
                    crate::scheduler::AgentEvent::TaskQueued { snapshot: queued },
                ))
                .await
                .unwrap();
            event_loop
                .handle_event(ReplEvent::AgentLifecycle(
                    crate::scheduler::AgentEvent::TaskStarted {
                        snapshot: snapshot.clone(),
                    },
                ))
                .await
                .unwrap();
            assert_eq!(
                output.get_messages().len(),
                1,
                "pre-result lifecycle delivery must remain pending while the provider spawn row can still claim it"
            );
            event_loop
                .handle_event(ReplEvent::AgentLifecycle(
                    crate::scheduler::AgentEvent::ToolStarted {
                        task_id,
                        name: "read".into(),
                    },
                ))
                .await
                .unwrap();
            event_loop
                .handle_event(ReplEvent::AgentLifecycle(
                    crate::scheduler::AgentEvent::ToolCompleted {
                        task_id,
                        name: "read".into(),
                        is_error: false,
                    },
                ))
                .await
                .unwrap();
            event_loop
                .handle_event(ReplEvent::AgentLifecycle(
                    crate::scheduler::AgentEvent::TaskFinished {
                        result: crate::scheduler::AgentTaskResult {
                            identity: identity.clone(),
                            status: crate::scheduler::AgentTaskStatus::Completed,
                            final_message: "grouping inspected".into(),
                            diagnostics: Vec::new(),
                            turns: 2,
                            elapsed_ms: 12,
                        },
                        usage: crate::scheduler::AgentUsage::default(),
                    },
                ))
                .await
                .unwrap();
            assert_eq!(
                output.get_messages().len(),
                1,
                "even a terminal child delivered before its spawn result must wait for the active provider row binding"
            );
            event_loop
                .handle_event(ReplEvent::ToolResult {
                    query_id,
                    round_token,
                    tool_id,
                    result: Ok(serde_json::to_string(&identity).unwrap()),
                })
                .await
                .unwrap();
            for late in [
                crate::scheduler::AgentEvent::ToolStarted {
                    task_id,
                    name: "late-read".into(),
                },
                lifecycle_task_finished(
                    identity,
                    crate::scheduler::AgentTaskStatus::Completed,
                    "grouping inspected",
                ),
            ] {
                event_loop
                    .handle_event(ReplEvent::AgentLifecycle(late))
                    .await
                    .unwrap();
            }

            let messages = output.get_messages();
            assert_eq!(
                messages.len(),
                1,
                "the spawn lifecycle must update its originating WorkUnit and append no loose terminal message; rendered={:?}",
                messages
                    .iter()
                    .map(|message| message.format(&crate::theme::ColorScheme::default()))
                    .collect::<Vec<_>>()
            );
            let rendered = messages[0].complete_transcript(&crate::theme::ColorScheme::default());
            for expected in ["spawn_agent", "inspect lifecycle grouping", "read", "grouping inspected"] {
                assert!(
                    rendered.contains(expected),
                    "the originating WorkUnit must retain {expected:?}; rendered={rendered:?}"
                );
            }
            assert_eq!(rendered.matches("grouping inspected").count(), 1);
            assert!(!rendered.contains("late-read"));
        }));
}

#[test]
fn test_issue_652_lifecycle_nested_interleaved_roots_keep_ownership() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(tokio::task::LocalSet::new().run_until(async {
            use crate::cli::messages::{Message, MessageStatus, };

            let (mut event_loop, output) = lifecycle_test_event_loop();
            let query_id = event_loop.query_states.create_query(Vec::new()).await;
            let first_tool_id = "spawn-first".to_string();
            let second_tool_id = "spawn-second".to_string();
            let round_token = event_loop
                .conversation
                .write()
                .await
                .stage_assistant(
                    query_id,
                    crate::providers::Message {
                        role: "assistant".into(),
                        content: vec![
                            crate::providers::ContentBlock::ToolUse {
                                id: first_tool_id.clone(),
                                name: "spawn_agent".into(),
                                input: serde_json::json!({"task": "first root task"}),
                            },
                            crate::providers::ContentBlock::ToolUse {
                                id: second_tool_id.clone(),
                                name: "spawn_agent".into(),
                                input: serde_json::json!({"task": "second root task"}),
                            },
                        ],
                    },
                )
                .unwrap();
            let unit = output.start_work_unit("Tools");
            let first_spawn = unit.add_row("spawn_agent(first root)");
            let second_spawn = unit.add_row("spawn_agent(second root)");

            let first_agent = uuid::Uuid::new_v4();
            let first = lifecycle_identity(
                first_agent,
                uuid::Uuid::new_v4(),
                None,
                first_agent,
                0,
            );
            let second_agent = uuid::Uuid::new_v4();
            let second = lifecycle_identity(
                second_agent,
                uuid::Uuid::new_v4(),
                None,
                second_agent,
                0,
            );
            let nested = lifecycle_identity(
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                Some(first.agent_id),
                first.agent_id,
                1,
            );
            event_loop.active_tool_uses.write().await.insert(
                first_tool_id.clone(),
                (
                    "spawn_agent".into(),
                    serde_json::json!({"task": "first root task"}),
                    Arc::clone(&unit),
                    first_spawn,
                ),
            );
            event_loop.active_tool_uses.write().await.insert(
                second_tool_id.clone(),
                (
                    "spawn_agent".into(),
                    serde_json::json!({"task": "second root task"}),
                    Arc::clone(&unit),
                    second_spawn,
                ),
            );
            for (tool_id, identity) in [
                (first_tool_id, first.clone()),
                (second_tool_id, second.clone()),
            ] {
                event_loop
                    .handle_event(ReplEvent::ToolResult {
                        query_id,
                        round_token,
                        tool_id,
                        result: Ok(serde_json::to_string(&identity).unwrap()),
                    })
                    .await
                    .unwrap();
            }
            assert_eq!(
                unit.status(),
                MessageStatus::InProgress,
                "successful spawn results must bind running child rows before retiring the provider tool round"
            );

            for event in [
                crate::scheduler::AgentEvent::TaskQueued {
                    snapshot: lifecycle_task_snapshot(
                        first.clone(),
                        "first root task",
                        crate::scheduler::AgentTaskStatus::Queued,
                    ),
                },
                crate::scheduler::AgentEvent::TaskQueued {
                    snapshot: lifecycle_task_snapshot(
                        second.clone(),
                        "second root task",
                        crate::scheduler::AgentTaskStatus::Queued,
                    ),
                },
                crate::scheduler::AgentEvent::TaskQueued {
                    snapshot: lifecycle_task_snapshot(
                        nested.clone(),
                        "nested task",
                        crate::scheduler::AgentTaskStatus::Queued,
                    ),
                },
                crate::scheduler::AgentEvent::TaskStarted {
                    snapshot: lifecycle_task_snapshot(
                        nested.clone(),
                        "nested task",
                        crate::scheduler::AgentTaskStatus::Running,
                    ),
                },
                crate::scheduler::AgentEvent::ToolStarted {
                    task_id: nested.task_id,
                    name: "grep".into(),
                },
                crate::scheduler::AgentEvent::ToolCompleted {
                    task_id: nested.task_id,
                    name: "grep".into(),
                    is_error: true,
                },
            ] {
                event_loop
                    .handle_event(ReplEvent::AgentLifecycle(event))
                    .await
                    .unwrap();
            }

            unit.set_complete();
            assert_eq!(
                unit.status(),
                MessageStatus::InProgress,
                "a provider completion must not retire scrollback while bound children can still update it"
            );

            event_loop
                .handle_event(ReplEvent::AgentLifecycle(lifecycle_task_finished(
                    nested.clone(),
                    crate::scheduler::AgentTaskStatus::Failed,
                    "nested failed cleanly",
                )))
                .await
                .unwrap();
            let after_nested_terminal =
                unit.complete_transcript(&crate::theme::ColorScheme::default());
            for late_nested in [
                crate::scheduler::AgentEvent::TaskQueued {
                    snapshot: lifecycle_task_snapshot(
                        nested.clone(),
                        "late nested queued",
                        crate::scheduler::AgentTaskStatus::Queued,
                    ),
                },
                crate::scheduler::AgentEvent::TaskStarted {
                    snapshot: lifecycle_task_snapshot(
                        nested.clone(),
                        "late nested started",
                        crate::scheduler::AgentTaskStatus::Running,
                    ),
                },
            ] {
                event_loop
                    .handle_event(ReplEvent::AgentLifecycle(late_nested))
                    .await
                    .unwrap();
            }
            assert_eq!(
                unit.complete_transcript(&crate::theme::ColorScheme::default()),
                after_nested_terminal,
                "terminal child B must reject late queued/started delivery while sibling A remains active"
            );

            for event in [
                lifecycle_task_finished(
                    first.clone(),
                    crate::scheduler::AgentTaskStatus::Failed,
                    "first failed cleanly",
                ),
                lifecycle_task_finished(
                    second.clone(),
                    crate::scheduler::AgentTaskStatus::Cancelled,
                    "second cancelled cleanly",
                ),
                // Duplicate terminal and late child-tool delivery are both no-ops.
                lifecycle_task_finished(
                    second.clone(),
                    crate::scheduler::AgentTaskStatus::Cancelled,
                    "second cancelled cleanly",
                ),
                crate::scheduler::AgentEvent::ToolStarted {
                    task_id: nested.task_id,
                    name: "late-tool".into(),
                },
                crate::scheduler::AgentEvent::TaskQueued {
                    snapshot: lifecycle_task_snapshot(
                        first.clone(),
                        "late queued root",
                        crate::scheduler::AgentTaskStatus::Queued,
                    ),
                },
            ] {
                event_loop
                    .handle_event(ReplEvent::AgentLifecycle(event))
                    .await
                    .unwrap();
            }

            assert_eq!(unit.status(), MessageStatus::Complete);
            assert_eq!(output.get_messages().len(), 1);
            let projected = crate::cli::test_projection::try_project_for_test(unit.as_ref(), &crate::theme::ColorScheme::default()).unwrap();
            assert_eq!(projected.role, crate::cli::test_projection::NodeRole::ToolGroup);
            assert_eq!(projected.children.len(), 2);
            let first_agent_row = projected.children[first_spawn]
                .children
                .iter()
                .find(|row| row.label.contains("first root task"))
                .expect("first spawn row must own the first root lifecycle");
            assert!(first_agent_row
                .children
                .iter()
                .any(|row| row.label.contains("nested task")));
            assert!(!projected.children[second_spawn]
                .children
                .iter()
                .any(|row| row.label.contains("nested task")));

            let rendered = unit.complete_transcript(&crate::theme::ColorScheme::default());
            for expected in [
                "first root task",
                "second root task",
                "nested task",
                "tool grep — failed",
                "first failed cleanly",
                "second cancelled cleanly",
            ] {
                assert!(
                    rendered.contains(expected),
                    "interleaved lifecycle content must remain in its originating group; missing={expected:?} rendered={rendered:?}"
                );
            }
            assert_eq!(rendered.matches("second cancelled cleanly").count(), 1);
            assert!(!rendered.contains("late-tool"));
            assert!(!rendered.contains("late queued root"));
            assert!(!rendered.contains("late nested"));
            assert!(
                !event_loop
                    .active_agent_root_tasks
                    .contains_key(&first.root_agent_id),
                "late queued delivery must not repopulate ownership for a terminal root"
            );
            assert!(
                event_loop.agent_lifecycle_bindings.is_empty()
                    && event_loop.agent_task_roots.is_empty()
                    && event_loop.active_agent_root_tasks.is_empty()
                    && event_loop.terminal_agent_tasks.is_empty(),
                "all lifecycle ownership maps and per-task tombstones must drain after sibling A terminalizes: bindings={:?} task_roots={:?} active={:?} terminal_tasks={:?}",
                event_loop.agent_lifecycle_bindings.keys().collect::<Vec<_>>(),
                event_loop.agent_task_roots,
                event_loop.active_agent_root_tasks,
                event_loop.terminal_agent_tasks,
            );

            for label in ["await_agent(second)", "cancel_agent(second)"] {
                let separate = output.start_work_unit("Tools");
                let row = separate.add_row(label);
                separate.complete_row(row, "complete");
                separate.set_complete();
            }
            assert_eq!(
                output.get_messages().len(),
                3,
                "await/cancel remain independent normal tool WorkUnits and never replace spawn lifecycle ownership"
            );
        }));
}

/// spawn_agent is gone from `active_tool_uses` when TaskFinished Failed
/// arrives: attach to the spawn parent Tools unit, not `Agent activity {uuid}`.
#[test]
fn test_spawn_agent_task_finished_failed_attaches_to_spawn_parent_not_new_root() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(tokio::task::LocalSet::new().run_until(async {

            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let query_id = event_loop.query_states.create_query(Vec::new()).await;
            let unit = output.start_work_unit("Tools");
            event_loop
                .query_states
                .set_tool_work_unit(query_id, Some(Arc::clone(&unit)))
                .await;
            let spawn = unit.add_row("spawn_agent(inspect max turns)");
            unit.complete_row(spawn, "spawned");

            let agent_id = uuid::Uuid::new_v4();
            let identity = lifecycle_identity(agent_id, uuid::Uuid::new_v4(), None, agent_id, 0);
            event_loop
                .handle_event(ReplEvent::AgentLifecycle(lifecycle_task_finished(
                    identity,
                    crate::scheduler::AgentTaskStatus::Failed,
                    "agent reached its turn limit without a final response: configured max_turns=3, consumed provider attempts=3",
                )))
                .await
                .unwrap();

            let messages = output.get_messages();
            let labels: Vec<String> = messages
                .iter()
                .filter_map(|message| {
                    crate::cli::test_projection::try_project_for_test(message.as_ref(), &crate::theme::ColorScheme::default())
                        .map(|row| row.label)
                })
                .collect();
            assert_eq!(
                messages.len(),
                1,
                "child TaskFinished Failed must not start a new root; labels={labels:?}"
            );
            assert!(
                labels.iter().all(|label| !label.contains("Agent activity")),
                "must not create Agent activity {{uuid}}; labels={labels:?}"
            );
            assert!(
                labels.iter().all(|label| {
                    !(label.contains(&agent_id.to_string()) && label.to_ascii_lowercase().contains("failed"))
                }),
                "compact root must not be child {{uuid}} Failed; labels={labels:?}"
            );
            let rendered = unit.complete_transcript(&crate::theme::ColorScheme::default());
            assert!(
                rendered.contains("spawn_agent"),
                "failure must remain on the spawn parent unit; rendered={rendered:?}"
            );
            assert!(
                rendered.contains("turn limit") || rendered.to_ascii_lowercase().contains("failed"),
                "spawn parent must show the child failure; rendered={rendered:?}"
            );
        }));
}

#[test]
fn test_issue_652_lifecycle_unbound_events_form_one_structured_activity_unit() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(tokio::task::LocalSet::new().run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            let agent_id = uuid::Uuid::new_v4();
            let identity = lifecycle_identity(agent_id, uuid::Uuid::new_v4(), None, agent_id, 0);
            for event in [
                crate::scheduler::AgentEvent::TaskQueued {
                    snapshot: lifecycle_task_snapshot(
                        identity.clone(),
                        "typed-program child",
                        crate::scheduler::AgentTaskStatus::Queued,
                    ),
                },
                crate::scheduler::AgentEvent::TaskStarted {
                    snapshot: lifecycle_task_snapshot(
                        identity.clone(),
                        "typed-program child",
                        crate::scheduler::AgentTaskStatus::Running,
                    ),
                },
                lifecycle_task_finished(
                    identity.clone(),
                    crate::scheduler::AgentTaskStatus::Completed,
                    "typed child done",
                ),
                lifecycle_task_finished(
                    identity,
                    crate::scheduler::AgentTaskStatus::Completed,
                    "typed child done",
                ),
            ] {
                event_loop
                    .handle_event(ReplEvent::AgentLifecycle(event))
                    .await
                    .unwrap();
            }

            let messages = output.get_messages();
            assert_eq!(messages.len(), 1);
            let projected = crate::cli::test_projection::try_project_for_test(
                messages[0].as_ref(),
                &crate::theme::ColorScheme::default(),
            )
            .unwrap();
            assert_eq!(
                projected.role,
                crate::cli::test_projection::NodeRole::Activity
            );
            assert_eq!(projected.children.len(), 1);
            assert!(projected.children[0].label.contains("typed-program child"));
            let rendered = messages[0].complete_transcript(&crate::theme::ColorScheme::default());
            assert_eq!(rendered.matches("typed child done").count(), 1);
        }));
}

#[test]
fn initialization_command_distinguishes_completed_one_shot() {
    let store = crate::brain::BrainStore::with_root("box.local", None);
    let attachment = store
        .attach(
            "shared",
            "alice",
            crate::brain::AttachmentRole::Driver,
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
    let message =
        initialization_schedule_message(&completed, Some(crate::brain::BrainRunStatus::Completed));
    assert!(message.contains("already completed"));
    assert!(!message.contains(" scheduled as "));
}

fn brain_event(
    seq: u64,
    sender: &str,
    kind: crate::brain::BrainEventKind,
) -> crate::brain::BrainEvent {
    crate::brain::BrainEvent {
        schema_version: 1,
        brain_id: crate::brain::BrainId(uuid::Uuid::nil()),
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
    use crate::brain::{AttachmentId, AttachmentRole, BrainEventKind, ConnectionId};

    let prompt = brain_event(
        1,
        "alice",
        BrainEventKind::Prompt {
            text: "hello".into(),
            attached_mentions: Vec::new(),
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
    let brain_id = crate::brain::BrainId(uuid::Uuid::new_v4());
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
    let first = crate::brain::BrainId(uuid::Uuid::new_v4());
    let second = crate::brain::BrainId(uuid::Uuid::new_v4());
    let mut revisions = std::collections::HashMap::new();

    assert!(advance_brain_projection_revision(&mut revisions, first, 20));
    assert!(advance_brain_projection_revision(&mut revisions, first, 21));
    assert!(advance_brain_projection_revision(&mut revisions, second, 1));
}

#[test]
fn canonical_brain_context_projects_conversation_without_program_source() {
    use crate::brain::{BrainEventKind, ProgramLanguage};
    use crate::cli::status_bar::{StatusBar, StatusLineType};

    let events = vec![
        brain_event(
            1,
            "alice",
            BrainEventKind::Prompt {
                text: "please compute forty two squared".into(),
                attached_mentions: Vec::new(),
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
fn project_brain_context_and_memtree_singleton_recaps_share_one_now_prefix() {
    use crate::brain::BrainEventKind;
    use crate::cli::status_bar::{StatusBar, StatusLineType};

    let status = StatusBar::new();
    status.update_line(StatusLineType::MemoryContext, "🧠 recalled 2");
    // What `refresh_context_strip` writes when MemTree returns one centroid line.
    status.update_line(
        StatusLineType::ContextLine(0),
        "   └─ now: I think Finch is unusually ambitious…",
    );

    super::project_brain_context(
        &status,
        &[brain_event(
            1,
            "shammah",
            BrainEventKind::ParticipantMessage {
                text: "hello".into(),
            },
        )],
        1,
        None,
    );

    let contents: Vec<String> = status
        .get_lines()
        .into_iter()
        .map(|line| line.content)
        .collect();
    let now_lines: Vec<&String> = contents
        .iter()
        .filter(|line| line.contains("└─ now:"))
        .collect();
    assert_eq!(
        now_lines.len(),
        1,
        "status recap must print └─ now: on exactly one line; got {contents:?}"
    );
    assert!(
        contents.iter().any(|line| {
            line.contains('💬') || line.contains('📋') || line.contains("├─")
        }),
        "the other recap line must be 💬 or 📋 (or ├─), never a second now:; got {contents:?}"
    );
    assert!(
        now_lines[0].contains("shammah: hello"),
        "the now: line must be the latest recap; now={now_lines:?} contents={contents:?}"
    );
}

#[test]
fn canonical_brain_context_ignores_failed_results_and_bounds_text() {
    use crate::brain::BrainEventKind;
    use crate::cli::status_bar::{StatusBar, StatusLineType};

    let events = vec![
        brain_event(
            1,
            "alice",
            BrainEventKind::Prompt {
                text: "a".repeat(100),
                attached_mentions: Vec::new(),
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
    use crate::brain::{
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
    use crate::brain::{
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
    use crate::brain::{BrainEventKind, BrainRunKind, BrainRunStatus, RunId};

    let output =
        crate::cli::output_manager::OutputManager::new(crate::theme::ColorScheme::default());
    output.disable_stdout();
    let run_id = RunId(uuid::Uuid::new_v4());
    let mut projections = std::collections::HashMap::new();
    super::ensure_remote_brain_run_projection(
        &output,
        &mut projections,
        run_id,
        Some(BrainRunKind::Speculative),
        BrainRunStatus::Running,
        None,
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
        &super::LocallyRenderedRuns::default(),
        None,
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
        &super::LocallyRenderedRuns::default(),
        None,
    ));

    let unit = projections.get(&run_id).unwrap().unit.clone();
    let projected = crate::cli::test_projection::try_project_for_test(
        unit.as_ref(),
        &crate::theme::ColorScheme::default(),
    )
    .unwrap();
    assert_eq!(
        projected.role,
        crate::cli::test_projection::NodeRole::Activity
    );
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
        .all(|row| row.role == crate::cli::test_projection::NodeRole::Activity));
    assert!(projected.children[0].label.contains("status"));
    assert!(projected.children[1].label.starts_with("result"));

    let canonical = unit.complete_transcript(&crate::theme::ColorScheme::default());
    assert!(canonical.contains("Speculative run"));
    assert!(canonical.contains("result"));
    assert!(canonical.contains("catalog unavailable"));
}

#[test]
fn named_brain_run_preserves_tool_semantics_inside_activity_group() {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };

    let output =
        crate::cli::output_manager::OutputManager::new(crate::theme::ColorScheme::default());
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
            &super::LocallyRenderedRuns::default(),
            None,
        ));
    }

    let unit = projections.get(&run_id).unwrap().unit.clone();
    let projected = crate::cli::test_projection::try_project_for_test(
        unit.as_ref(),
        &crate::theme::ColorScheme::default(),
    )
    .unwrap();
    assert_eq!(
        projected.role,
        crate::cli::test_projection::NodeRole::Activity
    );
    assert!(!projected.default_open);
    assert!(!projected.label.contains("Tools"));
    assert_eq!(projected.children.len(), 2);

    let status = &projected.children[0];
    assert_eq!(status.role, crate::cli::test_projection::NodeRole::Activity);
    assert_eq!(status.id.message_id, unit.id());
    assert_eq!(status.id.path, vec![1, 0]);

    let tool = &projected.children[1];
    assert_eq!(tool.role, crate::cli::test_projection::NodeRole::ToolCall);
    assert_eq!(tool.id.message_id, unit.id());
    assert_eq!(tool.id.path, vec![1, 1]);
    assert_eq!(tool.children.len(), 2);
    assert_eq!(
        tool.children[0].role,
        crate::cli::test_projection::NodeRole::Input
    );
    assert_eq!(tool.children[0].id.path, vec![1, 1, 0]);
    assert_eq!(
        tool.children[1].role,
        crate::cli::test_projection::NodeRole::ToolOutput
    );
    assert_eq!(tool.children[1].id.path, vec![1, 1, 1]);
    assert!(tool.children[1].body.iter().any(|line| line == "value=7"));

    let canonical = unit.complete_transcript(&crate::theme::ColorScheme::default());
    assert!(canonical.contains("read_cache"));
    assert!(canonical.contains("cache hit"));
    assert!(canonical.contains("value=7"));
}

/// The replayed-event pattern for one interactive say turn, in journal order.
fn replayed_say_run_events(
    run_id: crate::brain::RunId,
    source: &str,
    output: Option<&str>,
    error: Option<String>,
    terminal: Option<crate::brain::BrainRunStatus>,
) -> Vec<crate::brain::BrainEvent> {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, ProgramLanguage,
    };
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Interactive,
        parent_run_id: None,
        request_seq: 1,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "shammah".into(),
        status: BrainRunStatus::Running,
        started_ms: 1,
        updated_ms: 1,
        detail: None,
    };
    let mut kinds = vec![
        BrainEventKind::RunStarted { run },
        BrainEventKind::Program {
            language: ProgramLanguage::Lisp,
            source: source.to_string(),
        },
    ];
    if let Some(text) = output {
        kinds.push(BrainEventKind::Result {
            request_seq: 1,
            output: text.to_string(),
            error,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        });
    }
    if let Some(status) = terminal {
        kinds.push(BrainEventKind::RunStatusChanged {
            run_id,
            status,
            detail: None,
        });
    }
    kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            let mut event = brain_event(index as u64 + 1, "daemon", kind);
            event.run_id = Some(run_id);
            event
        })
        .collect()
}

/// The journal pattern of one typed-program (`(say …)` pushed at the prompt)
/// interactive run: the run-unaffiliated Program event AT the run's
/// request_seq is the turn's wire source, the runner's runtime commit rides
/// the run, and the Result carries the output. Production journals this
/// exact shape (see the #970 PTY fixture dump); there is no run-correlated
/// Program event on this path.
#[allow(clippy::too_many_arguments)]
fn replayed_typed_program_say_events(
    run_id: crate::brain::RunId,
    source: &str,
    output: Option<&str>,
    error: Option<String>,
    terminal: Option<crate::brain::BrainRunStatus>,
) -> Vec<crate::brain::BrainEvent> {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, ProgramLanguage,
    };
    let mut kinds = vec![BrainEventKind::Program {
        language: ProgramLanguage::Lisp,
        source: source.to_string(),
    }];
    let request_seq = 1u64;
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Interactive,
        parent_run_id: None,
        request_seq,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "shammah".into(),
        status: BrainRunStatus::Running,
        started_ms: 1,
        updated_ms: 1,
        detail: None,
    };
    kinds.push(BrainEventKind::RunStarted { run });
    kinds.push(BrainEventKind::RuntimeCommitted {
        request_seq,
        runtime_revision: 1,
        checkpoint_sha256: "abc".into(),
    });
    if let Some(text) = output {
        kinds.push(BrainEventKind::Result {
            request_seq,
            output: text.to_string(),
            error,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        });
    }
    if let Some(status) = terminal {
        kinds.push(BrainEventKind::RunStatusChanged {
            run_id,
            status,
            detail: None,
        });
    }
    kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            let mut event = brain_event(index as u64 + 1, "daemon", kind);
            if index == 0 {
                event.sender = "shammah".into();
                event.run_id = None;
            } else {
                event.run_id = Some(run_id);
            }
            event
        })
        .collect()
}

fn replay_output_manager() -> crate::cli::output_manager::OutputManager {
    let output =
        crate::cli::output_manager::OutputManager::new(crate::theme::ColorScheme::default());
    output.disable_stdout();
    output
}

/// #970 regression (replay projection): a completed say turn's journal
/// pattern — one typed Program + a successful Result for the same interactive
/// run — rebuilds the component ViewModel on the replayed run unit, while the
/// legacy rows stay on the unit as the canonical record.
#[test]
fn replayed_completed_say_run_reconstructs_the_component_card() {
    use crate::cli::messages::{Message, SayTurnStatus};

    let greeting = "Hi, Shammah! What would you like to work on?";
    let source = format!("(say \"{greeting}\")");
    let run_id = crate::brain::RunId(uuid::Uuid::new_v4());
    let events = replayed_say_run_events(
        run_id,
        &source,
        Some(greeting),
        None,
        Some(crate::brain::BrainRunStatus::Completed),
    );

    let output = replay_output_manager();
    let mut projections = std::collections::HashMap::new();
    let mut local_projections = std::collections::VecDeque::new();
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
    );

    let unit = projections
        .get(&run_id)
        .unwrap_or_else(|| panic!("INVARIANT: the replayed run must project a WorkUnit"))
        .unit
        .clone();
    let view = unit.say_turn_view().unwrap_or_else(|| {
        panic!(
            "INVARIANT: the replayed completed say turn must reconstruct its component \
             ViewModel (say_vm) from the journal pattern; unit={:?}",
            unit.domain_head()
        )
    });
    assert_eq!(
        view.vm.status,
        SayTurnStatus::Completed,
        "INVARIANT: the run ended Completed, so the replayed card is Completed; view={view:?}"
    );
    assert_eq!(
        view.vm.program.lines,
        source.lines().map(str::to_owned).collect::<Vec<_>>(),
        "INVARIANT: the card retains the raw wire source for the toggle; view={view:?}"
    );
    let output_lines = view
        .vm
        .output
        .as_ref()
        .unwrap_or_else(|| panic!("INVARIANT: the Result output must fill the card's output part"))
        .lines
        .clone();
    assert!(
        output_lines.iter().any(|line| line == greeting),
        "INVARIANT: the replayed card renders the say prose; output_lines={output_lines:?}"
    );

    // The viewport renders the card: prose + `(ran Ns)`, no legacy rows.
    let card: Vec<String> = finch_ui_model::say_turn_lines(&view)
        .into_iter()
        .map(|line| line.text)
        .collect();
    assert!(
        card.iter().any(|line| line.contains(greeting)),
        "INVARIANT: the replayed card renders the completed prose; card={card:?}"
    );
    assert!(
        card.iter().any(|line| line.contains("(ran ")),
        "INVARIANT: the replayed card carries the `(ran Ns)` annotation (stage-2 \
         verbatim target, docs/TUI_DESIGN.md); card={card:?}"
    );
    assert!(
        !card.iter().any(|line| line.contains("Program source")),
        "INVARIANT: the replayed viewport must not render the legacy Program source \
         row; card={card:?}"
    );
    assert!(
        !card.iter().any(|line| line.contains("result")),
        "INVARIANT: the replayed viewport must not render the legacy result row; \
         card={card:?}"
    );

    // The toggle works on the replayed card: the output region routes the
    // component action and swaps the prose to the source.
    let action = unit
        .say_turn_action(&[1])
        .expect("INVARIANT: the replayed card's output region is the toggle hit target");
    assert!(
        unit.handle_say_turn_action(&action),
        "INVARIANT: the replayed card must route the toggle through its ViewModel"
    );
    assert!(
        unit.say_turn_view()
            .unwrap_or_else(|| { panic!("INVARIANT: the say ViewModel must survive the toggle") })
            .vm
            .show_program,
        "INVARIANT: the toggle flips show_program on the replayed card"
    );

    // The canonical record is unchanged: the raw program and output stay on
    // the unit's legacy rows, spooling exactly once.
    let canonical = unit.complete_transcript(&crate::theme::ColorScheme::default());
    assert!(
        canonical.contains(&source),
        "INVARIANT: the canonical record keeps the raw program; canonical=\n{canonical}"
    );
    assert!(
        canonical.contains(greeting),
        "INVARIANT: the canonical record keeps the say output; canonical=\n{canonical}"
    );
}

/// #970 acceptance: a say turn mid-stream at disconnect replays as its
/// best-known state — source inline while running, prose beneath once the
/// output arrived even before the run reported terminal status.
#[test]
fn replayed_midstream_say_run_replays_best_known_state() {
    use crate::cli::messages::{Message, SayTurnStatus};

    let greeting = "Hi, Shammah!";
    let source = format!("(say \"{greeting}\")");

    // Program emitted, no Result yet: the card renders the source inline.
    let run_id = crate::brain::RunId(uuid::Uuid::new_v4());
    let events = replayed_say_run_events(run_id, &source, None, None, None);
    let output = replay_output_manager();
    let mut projections = std::collections::HashMap::new();
    let mut local_projections = std::collections::VecDeque::new();
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
    );
    let unit = projections.get(&run_id).expect("run unit").unit.clone();
    let view = unit
        .say_turn_view()
        .expect("INVARIANT: a replayed running say turn reconstructs its card");
    assert_eq!(
        view.vm.status,
        SayTurnStatus::Running,
        "INVARIANT: without a terminal status the replayed card stays Running; view={view:?}"
    );
    assert!(
        view.vm.output.is_none(),
        "INVARIANT: no Result arrived, so the card has no output part; view={view:?}"
    );
    let card: Vec<String> = finch_ui_model::say_turn_lines(&view)
        .into_iter()
        .map(|line| line.text)
        .collect();
    assert!(
        card.iter().any(|line| line.contains(&source)),
        "INVARIANT: the Running card renders the program source inline; card={card:?}"
    );

    // Output arrived, run still running: prose renders beneath the source.
    let run_id = crate::brain::RunId(uuid::Uuid::new_v4());
    let events = replayed_say_run_events(run_id, &source, Some(greeting), None, None);
    let output = replay_output_manager();
    let mut projections = std::collections::HashMap::new();
    let mut local_projections = std::collections::VecDeque::new();
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
    );
    let unit = projections.get(&run_id).expect("run unit").unit.clone();
    let view = unit
        .say_turn_view()
        .expect("INVARIANT: a replayed running say turn reconstructs its card");
    assert_eq!(
        view.vm.status,
        SayTurnStatus::Running,
        "INVARIANT: the run has not reported terminal status, so the card stays \
         Running; view={view:?}"
    );
    let arrived = view
        .vm
        .output
        .as_ref()
        .expect("INVARIANT: the arrived Result output must fill the card")
        .lines
        .clone();
    assert!(
        arrived.iter().any(|line| line == greeting),
        "INVARIANT: arrived say bytes are never hidden; output={arrived:?}"
    );
    let card: Vec<String> = finch_ui_model::say_turn_lines(&view)
        .into_iter()
        .map(|line| line.text)
        .collect();
    let card_text = card
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        card_text.contains(&source) && card_text.contains(greeting),
        "INVARIANT: the Running card renders the source inline with the arrived \
         prose beneath it; card={card:?}"
    );
}

/// A run that carried tool calls is more than one say turn: the legacy
/// projection (which renders the tool rows) must survive reconstruction.
#[test]
fn replayed_say_run_with_tools_keeps_the_legacy_projection() {
    use crate::brain::{BrainEventKind, BrainRunStatus};
    use crate::cli::messages::Message;

    let source = "(say \"hello\")";
    let run_id = crate::brain::RunId(uuid::Uuid::new_v4());
    let mut events = replayed_say_run_events(
        run_id,
        source,
        Some("hello"),
        None,
        Some(BrainRunStatus::Completed),
    );
    // Splice a tool round between the Program and the Result, then renumber
    // the journal sequence so the list stays in journal order.
    let tool_events = [
        BrainEventKind::ToolCall {
            request_seq: 1,
            tool_id: "tool-970".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "src/main.rs"}),
        },
        BrainEventKind::ToolResult {
            request_seq: 1,
            tool_id: "tool-970".into(),
            output: "fn main() {}".into(),
            is_error: false,
        },
    ];
    for (offset, kind) in tool_events.into_iter().enumerate() {
        let mut event = brain_event(0, "provider", kind);
        event.run_id = Some(run_id);
        events.insert(2 + offset, event);
    }
    for (index, event) in events.iter_mut().enumerate() {
        event.seq = index as u64 + 1;
    }

    let output = replay_output_manager();
    let mut projections = std::collections::HashMap::new();
    let mut local_projections = std::collections::VecDeque::new();
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
    );
    let unit = projections.get(&run_id).expect("run unit").unit.clone();
    assert!(
        unit.say_turn_view().is_none(),
        "INVARIANT: a run with tool calls keeps the legacy projection — the card \
         would hide the tool rows; unit={:?}",
        unit.domain_head()
    );
    let canonical = unit.complete_transcript(&crate::theme::ColorScheme::default());
    assert!(
        canonical.contains("read_file"),
        "INVARIANT: the tool row still renders through the legacy projection; \
         canonical=\n{canonical}"
    );
}

/// An errored Result is a failed turn, not a say card: the legacy projection
/// renders the failure.
#[test]
fn replayed_errored_say_result_keeps_the_legacy_projection() {
    use crate::brain::BrainRunStatus;
    use crate::cli::messages::Message;

    let source = "(say \"hello\")";
    let run_id = crate::brain::RunId(uuid::Uuid::new_v4());
    let events = replayed_say_run_events(
        run_id,
        source,
        Some(""),
        Some("VM wire error: E-WIRE-001".to_string()),
        Some(BrainRunStatus::Failed),
    );
    let output = replay_output_manager();
    let mut projections = std::collections::HashMap::new();
    let mut local_projections = std::collections::VecDeque::new();
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
    );
    let unit = projections.get(&run_id).expect("run unit").unit.clone();
    assert!(
        unit.say_turn_view().is_none(),
        "INVARIANT: an errored Result is not a say card; unit={:?}",
        unit.domain_head()
    );
    let canonical = unit.complete_transcript(&crate::theme::ColorScheme::default());
    assert!(
        canonical.contains("VM wire error: E-WIRE-001"),
        "INVARIANT: the failed result still renders through the legacy projection; \
         canonical=\n{canonical}"
    );
}

/// Non-interactive runs (speculative helper turns, scheduled deliveries) keep
/// the durable legacy projection: the say card is the interactive turn's
/// representation.
#[test]
fn replayed_non_interactive_run_keeps_the_legacy_projection() {
    use crate::brain::{BrainEventKind, BrainRunStatus};
    use crate::cli::messages::Message;

    let source = "(say \"hello\")";
    let run_id = crate::brain::RunId(uuid::Uuid::new_v4());
    let mut events = replayed_say_run_events(
        run_id,
        source,
        Some("hello"),
        None,
        Some(BrainRunStatus::Completed),
    );
    // Rewrite the run's kind to Speculative in place.
    let mut speculative = events.remove(0);
    let BrainEventKind::RunStarted { run } = &mut speculative.kind else {
        panic!(
            "fixture must start with RunStarted; got {:?}",
            speculative.kind
        )
    };
    run.kind = crate::brain::BrainRunKind::Speculative;
    events.insert(0, speculative);

    let output = replay_output_manager();
    let mut projections = std::collections::HashMap::new();
    let mut local_projections = std::collections::VecDeque::new();
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
    );
    let unit = projections.get(&run_id).expect("run unit").unit.clone();
    assert!(
        unit.say_turn_view().is_none(),
        "INVARIANT: a speculative run keeps the legacy projection; unit={:?}",
        unit.domain_head()
    );
}

/// Idempotence: a second snapshot of the same run (reconnect over reconnect)
/// must not reset the reader's `show_program` choice or duplicate the output
/// bytes in the ViewModel.
#[test]
fn replayed_say_card_survives_a_second_snapshot_without_duplicating_output() {
    use crate::cli::messages::{Message, SayTurnStatus};

    let greeting = "Hi!";
    let source = format!("(say \"{greeting}\")");
    let run_id = crate::brain::RunId(uuid::Uuid::new_v4());
    let events = replayed_say_run_events(
        run_id,
        &source,
        Some(greeting),
        None,
        Some(crate::brain::BrainRunStatus::Completed),
    );

    let output = replay_output_manager();
    let mut projections = std::collections::HashMap::new();
    let mut local_projections = std::collections::VecDeque::new();
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
    );
    let unit = projections.get(&run_id).expect("run unit").unit.clone();
    let action = unit.say_turn_action(&[1]).expect("toggle target");
    assert!(unit.handle_say_turn_action(&action), "first toggle");
    assert!(
        unit.say_turn_view().expect("card").vm.show_program,
        "fixture: the reader toggled the card to the source"
    );

    // The reconnect delivers the same journal suffix again.
    let mut local_projections = std::collections::VecDeque::new();
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
    );
    let view = unit
        .say_turn_view()
        .expect("INVARIANT: the card survives the second snapshot");
    assert!(
        view.vm.show_program,
        "INVARIANT: the second snapshot must not reset the reader's show_program \
         choice; view={view:?}"
    );
    let output_lines = view.vm.output.as_ref().expect("output stays").lines.clone();
    assert_eq!(
        output_lines.iter().filter(|line| *line == greeting).count(),
        1,
        "INVARIANT: the second snapshot must not duplicate the output bytes; \
         output_lines={output_lines:?}"
    );
    assert_eq!(
        view.vm.status,
        SayTurnStatus::Completed,
        "INVARIANT: the card's state is unchanged by the second snapshot"
    );
}

fn replayed_brain_snapshot(events: Vec<crate::brain::BrainEvent>) -> crate::brain::BrainSnapshot {
    crate::brain::BrainSnapshot {
        brain_id: crate::brain::BrainId(uuid::Uuid::new_v4()),
        name: "shared".into(),
        environment: crate::brain::BrainEnvironment {
            machine: "box.local".into(),
            workspace: std::path::PathBuf::from("/tmp"),
            generation: 1,
        },
        revision: events.iter().map(|event| event.seq).max().unwrap_or(0),
        events,
        program_stack: Vec::new(),
        attachments: Vec::new(),
        runner_lease: None,
        runner_handoff: None,
        runs: Vec::new(),
        tasks: Vec::new(),
        committed_memories: Vec::new(),
        schedules: Vec::new(),
        pending_schedule_dues: Vec::new(),
        effect_audits: Vec::new(),
    }
}

/// Production-boundary regression for #970's guest-replay duplication: when a
/// replayed run's say card already carries the turn's program, the separate
/// run-unaffiliated Program source unit must not render beside it — the same
/// turn must not wear two representations in one session.
#[tokio::test]
async fn reattached_say_snapshot_renders_one_card_without_duplicate_source_unit() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::brain::BrainRunStatus;
            use crate::cli::messages::MessageStatus;

            let greeting = "Hi, Shammah!";
            let source = format!("(say \"{greeting}\")");
            let run_id = crate::brain::RunId(uuid::Uuid::new_v4());
            let events = replayed_typed_program_say_events(
                run_id,
                &source,
                Some(greeting),
                None,
                Some(BrainRunStatus::Completed),
            );

            let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
            let tempdir = tempfile::tempdir().expect("say replay fixture: isolated tool state");
            let executor = crate::tools::ToolExecutor::new(
                crate::tools::ToolRegistry::new(),
                crate::tools::PermissionManager::new(),
                tempdir.path().join("patterns.json"),
            )
            .expect("say replay fixture: construct inert tool executor");
            let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
            let mut event_loop = super::EventLoop::new_named_brain_test_runner(
                generator,
                Vec::new(),
                Arc::new(tokio::sync::Mutex::new(executor)),
                Arc::clone(&runtime),
            );
            event_loop.output_manager.disable_stdout();

            event_loop
                .render_remote_brain_message(crate::brain::BrainWireMessage::Snapshot {
                    brain: replayed_brain_snapshot(events),
                })
                .await
                .expect("snapshot replay must dispatch");

            let messages = event_loop.output_manager.get_messages();
            let rendered = messages
                .iter()
                .map(|message| {
                    (
                        message.id(),
                        message.format(&crate::theme::ColorScheme::default()),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                messages.len(),
                1,
                "INVARIANT: the replayed say turn renders one representation — the card on \
         the run group; the run-unaffiliated source unit must not duplicate it. \
         messages={rendered:?}"
            );
            let unit = messages[0].say_turn_view().unwrap_or_else(|| {
                panic!(
                    "INVARIANT: the replayed run group must carry the say card; \
                 messages={rendered:?}"
                )
            });
            assert_eq!(
                unit.vm.status,
                crate::cli::messages::SayTurnStatus::Completed,
                "INVARIANT: the completed run replays as a completed card; view={unit:?}"
            );
            assert_eq!(
                messages[0].status(),
                MessageStatus::Complete,
                "INVARIANT: the run unit is complete so the card commits exactly once; \
         rendered={rendered:?}"
            );
            let canonical = messages[0].complete_transcript(&crate::theme::ColorScheme::default());
            assert!(
                canonical.contains(&source) && canonical.contains(greeting),
                "INVARIANT: the canonical record keeps the raw program and the say output \
         exactly once through the run group's rows; canonical=\n{canonical}"
            );
        })
        .await;
}

/// The duplicate-source suppression is keyed on reconstructed say cards: a
/// program whose run kept the legacy projection (here: a tool round) still
/// renders its run-unaffiliated source unit.
#[tokio::test]
async fn reattached_non_say_program_still_renders_its_source_unit() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::brain::{BrainEventKind, BrainRunStatus};

            let source = "(+ 970 0)";
            let run_id = crate::brain::RunId(uuid::Uuid::new_v4());
            let mut events = replayed_typed_program_say_events(
                run_id,
                source,
                Some("970"),
                None,
                Some(BrainRunStatus::Completed),
            );
            let tool_call = {
                let mut event = brain_event(
                    0,
                    "provider",
                    BrainEventKind::ToolCall {
                        request_seq: 1,
                        tool_id: "tool-970".into(),
                        name: "read_file".into(),
                        input: serde_json::json!({"path": "src/main.rs"}),
                    },
                );
                event.run_id = Some(run_id);
                event
            };
            events.insert(2, tool_call);
            for (index, event) in events.iter_mut().enumerate() {
                event.seq = index as u64 + 1;
            }

            let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
            let tempdir =
                tempfile::tempdir().expect("say replay control fixture: isolated tool state");
            let executor = crate::tools::ToolExecutor::new(
                crate::tools::ToolRegistry::new(),
                crate::tools::PermissionManager::new(),
                tempdir.path().join("patterns.json"),
            )
            .expect("say replay control fixture: construct inert tool executor");
            let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
            let mut event_loop = super::EventLoop::new_named_brain_test_runner(
                generator,
                Vec::new(),
                Arc::new(tokio::sync::Mutex::new(executor)),
                Arc::clone(&runtime),
            );
            event_loop.output_manager.disable_stdout();

            event_loop
                .render_remote_brain_message(crate::brain::BrainWireMessage::Snapshot {
                    brain: replayed_brain_snapshot(events),
                })
                .await
                .expect("snapshot replay must dispatch");

            let messages = event_loop.output_manager.get_messages();
            let rendered = messages
                .iter()
                .map(|message| message.format(&crate::theme::ColorScheme::default()))
                .collect::<Vec<_>>()
                .join("\n---\n");
            assert!(
                messages.iter().any(|message| {
                    message.work_unit_head().is_some_and(|head| {
                        matches!(
                            head.presentation,
                            crate::cli::messages::WorkUnitPresentation::ProgramSource { .. }
                        )
                    })
                }),
                "INVARIANT: an uncovered program keeps its run-unaffiliated source unit in \
         the replayed transcript; rendered=\n{rendered}"
            );
            assert!(
                messages
                    .iter()
                    .all(|message| message.say_turn_view().is_none()),
                "INVARIANT: a tool-carrying run keeps the legacy projection — no say card; \
         rendered=\n{rendered}"
            );
        })
        .await;
}

#[test]
fn todo_write_transcript_shows_the_task_list_not_the_raw_json() {
    // Issue #425 (todo_write dumps raw JSON as transcript rows): one todo_write
    // produced a row whose label was the tool's raw input JSON, and the task
    // list itself was never shown. The brain event projection must render the
    // todo payload as the readable list it represents — in the call row and in
    // the approval row — and never splice the JSON into the transcript.
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };

    // The payload reported in the issue: a four-task todo_write input.
    let todos = serde_json::json!({"todos": [
        {"content": "Identify the harness implementation, website source, and deployment target",
         "id": "1", "priority": "high", "status": "in_progress"},
        {"content": "Implement the typed program runner in the harness",
         "id": "2", "priority": "high", "status": "pending"},
        {"content": "Build the website source from the harness output",
         "id": "3", "priority": "medium", "status": "pending"},
        {"content": "Verify the deployment target accepts the build",
         "id": "4", "priority": "low", "status": "completed"}
    ]});
    let payload = todos.to_string();

    let output =
        crate::cli::output_manager::OutputManager::new(crate::theme::ColorScheme::default());
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
            tool_id: "call_425".into(),
            name: "todo_write".into(),
            input: todos.clone(),
        },
        BrainEventKind::ApprovalRequested {
            request_seq: 1,
            approval_id: "approval_425".into(),
            approval_kind: "tool".into(),
            subject: "todo_write".into(),
            audience: None,
            detail: serde_json::json!({"input": todos}),
        },
        BrainEventKind::ToolResult {
            request_seq: 1,
            tool_id: "call_425".into(),
            output: "Todo list updated: 4 tasks (1 in_progress, 2 pending, 1 completed)".into(),
            is_error: false,
        },
        BrainEventKind::ApprovalDecided {
            request_seq: 1,
            approval_id: "approval_425".into(),
            decision: serde_json::json!({"choice": "approve_pattern_session"}),
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
        assert!(
            super::project_remote_brain_run_event(
                &output,
                &mut projections,
                &event,
                &super::LocallyRenderedRuns::default(),
                None,
            ),
            "every projected run event must be acknowledged: seq={}",
            index + 1
        );
    }

    let unit = projections.get(&run_id).unwrap().unit.clone();
    let projected = crate::cli::test_projection::try_project_for_test(
        unit.as_ref(),
        &crate::theme::ColorScheme::default(),
    )
    .unwrap();

    // The todo_write call is one tool row labelled by the task list, not by raw
    // JSON; its output body carries the list lines durably.
    let tool = projected
        .children
        .iter()
        .find(|row| row.role == crate::cli::test_projection::NodeRole::ToolCall)
        .expect("the todo_write call must project as a tool row");
    let tool_dump = format!("label={:?} children={:?}", tool.label, tool.children);
    assert!(
        tool.label.contains("Task list"),
        "the call row must be labelled by the task list, not the tool's raw JSON: {tool_dump} payload={payload}"
    );
    assert!(
        !tool.label.contains('{'),
        "raw JSON must never be spliced into a row label: {tool_dump} payload={payload}"
    );
    let tool_body: String = tool
        .children
        .iter()
        .flat_map(|child| child.body.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        tool_body.contains("[in progress] Identify the harness implementation, website source, and deployment target")
            && tool_body.contains("[pending] Implement the typed program runner in the harness")
            && tool_body.contains("[pending] Build the website source from the harness output")
            && tool_body.contains("[completed] Verify the deployment target accepts the build"),
        "the transcript must show the task list with statuses: {tool_body} payload={payload}"
    );

    // The approval row renders the payload as the list it represents too — the
    // pretty-printed detail dump must be gone.
    let approval = projected
        .children
        .iter()
        .find(|row| row.label.starts_with("approval"))
        .expect("the approval must project as a row");
    let approval_body = approval.body.join("\n");
    assert!(
        approval_body.contains("[in progress] Identify the harness implementation, website source, and deployment target"),
        "the approval row must render the task list, not the detail JSON: {approval_body} payload={payload}"
    );
    assert!(
        !approval_body.contains('{'),
        "raw JSON must never be printed as an approval row body: {approval_body} payload={payload}"
    );

    let canonical = unit.complete_transcript(&crate::theme::ColorScheme::default());
    let canonical_payload = format!("transcript:\n{canonical}\npayload={payload}");
    assert!(
        !canonical.contains('{') && !canonical.contains("\"todos\""),
        "no raw JSON may survive anywhere in the projected transcript: {canonical_payload}"
    );
}

#[test]
fn snapshot_first_home_reconnect_reconciles_one_complete_work_unit() {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, ProgramLanguage,
        RunId,
    };

    let output =
        crate::cli::output_manager::OutputManager::new(crate::theme::ColorScheme::default());
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
        None,
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
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
    );
    assert!(local_projections.is_empty());

    let messages = output.get_messages();
    assert_eq!(messages.len(), 1);
    let rendered = messages[0].format(&crate::theme::ColorScheme::default());
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
    let initial = crate::providers::Message::user("prompt");
    let continuation = crate::providers::Message::with_content(
        "assistant",
        vec![
            crate::providers::ContentBlock::text("(say \""),
            crate::providers::ContentBlock::opaque_reasoning("opaque-between-text"),
            crate::providers::ContentBlock::text("done\")"),
        ],
    );
    let (source, language, captured) =
        super::named_brain_wire_source(vec![initial, continuation.clone()], 1).unwrap();
    assert_eq!(source, "(say \"done\")");
    assert_eq!(language, crate::brain::ProgramLanguage::Lisp);
    assert_eq!(
        serde_json::to_value(captured).unwrap(),
        serde_json::to_value(vec![continuation]).unwrap()
    );
}

#[test]
fn missing_final_wire_after_home_tool_rounds_reconciles_durable_error() {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };

    let output =
        crate::cli::output_manager::OutputManager::new(crate::theme::ColorScheme::default());
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
        None,
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
            &super::LocallyRenderedRuns::default(),
            &mut std::collections::HashSet::new(),
            None,
        ));
    }
    assert!(local_projections.is_empty());
    super::project_remote_brain_snapshot_runs(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &events,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
    );

    let messages = output.get_messages();
    assert_eq!(messages.len(), 1);
    let rendered = messages[0].format(&crate::theme::ColorScheme::default());
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
fn named_brain_live_result_keeps_assistant_prose_say() {
    // Production dump: local `(say "Hi, …")` became Program source with a
    // collapsed `result` child because live Result delivery deleted the
    // assistant-prose output unit (#820).
    use crate::brain::{BrainEventKind, BrainRunKind, BrainRunStatus, ProgramLanguage, RunId};

    let greeting = "Hi, Shammah! What would you like to work on?";
    let source = format!("(say \"{greeting}\")");
    let output =
        crate::cli::output_manager::OutputManager::new(crate::theme::ColorScheme::default());
    output.disable_stdout();
    let run_id = RunId(uuid::Uuid::new_v4());
    let mut projections = std::collections::HashMap::new();
    let run_unit = super::ensure_remote_brain_run_projection(
        &output,
        &mut projections,
        run_id,
        Some(BrainRunKind::Interactive),
        BrainRunStatus::Running,
        None,
    )
    .unit
    .clone();
    run_unit.set_program_source("lisp");
    run_unit.set_response(&source);
    run_unit.set_complete();

    let say = output.start_work_unit("VM program output");
    say.set_program_output();
    say.append_response(greeting);
    say.present_as_assistant_prose();
    say.set_complete();
    assert_eq!(
        output.get_messages().len(),
        2,
        "fixture is the dump's two-row turn: Program source plus say output"
    );
    assert!(
        say.is_assistant_prose(),
        "fixture must be the 804 prose mark; otherwise this test cannot prove #820"
    );

    let mut local_projections = std::collections::VecDeque::from([LocalBrainProjection {
        run_id,
        source: source.clone(),
        output: greeting.to_string(),
        tool_ids: std::collections::HashSet::new(),
        approval_ids: std::collections::HashSet::new(),
        program_seq: None,
        transient_output_unit: Some(say.clone()),
        failed: false,
    }]);

    let mut program = brain_event(
        12,
        "provider",
        BrainEventKind::Program {
            language: ProgramLanguage::Lisp,
            source: source.clone(),
        },
    );
    program.run_id = Some(run_id);
    assert!(super::project_remote_brain_live_run_event(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &program,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
        None,
    ));

    let mut result = brain_event(
        14,
        "daemon",
        BrainEventKind::Result {
            request_seq: 12,
            output: greeting.to_string(),
            error: None,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
    );
    result.run_id = Some(run_id);
    assert!(super::project_remote_brain_live_run_event(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &result,
        &super::LocallyRenderedRuns::default(),
        &mut std::collections::HashSet::new(),
        None,
    ));

    let messages = output.get_messages();
    let ids: Vec<_> = messages.iter().map(|message| message.id()).collect();
    let haystack = messages
        .iter()
        .map(|message| message.format(&crate::theme::ColorScheme::default()))
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert!(
        ids.contains(&say.id()),
        "invariant: a matching daemon Result must not delete untitled say prose; \
         that is the dump where Program source stayed and the greeting hid under \
         collapsed result (#820); ids={ids:?}; haystack={haystack}"
    );
    assert!(
        say.is_assistant_prose() && haystack.contains(greeting),
        "invariant: the surviving row is still the greeting, not an empty husk; \
         haystack={haystack}"
    );
}

#[test]
fn local_runner_projection_suppresses_matching_canonical_program_and_result() {
    let mut projection = LocalBrainProjection {
        run_id: crate::brain::RunId(uuid::Uuid::nil()),
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
        crate::brain::BrainEventKind::Program {
            language: crate::brain::ProgramLanguage::Lisp,
            source: "(say \"hello\")".into(),
        },
    );
    program.run_id = Some(projection.run_id);
    assert_eq!(projection.observe(&program), LocalProjectionMatch::Suppress);
    assert_eq!(projection.program_seq, Some(12));

    let mut result = brain_event(
        14,
        "daemon",
        crate::brain::BrainEventKind::Result {
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
        run_id: crate::brain::RunId(uuid::Uuid::nil()),
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
        crate::brain::BrainEventKind::Result {
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

/// #978 regression at the delegation boundary: a typed program the daemon
/// hands to this frontend's runner lease renders the card through the local
/// units — no Brain run group, no duplicate rows — and the daemon's terminal
/// events for the run paint nothing beside it.
#[tokio::test]
async fn delegated_program_say_renders_card_without_run_group_or_duplicate_rows() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::brain::{BrainEventKind, ProgramLanguage, RunId};
            use crate::cli::messages::SayTurnStatus;

            let greeting = "Hello, daemon say turn";
            let source = format!("(say \"{greeting}\")");
            let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
            let tempdir = tempfile::tempdir().expect("delegated say fixture: isolated tool state");
            let executor = crate::tools::ToolExecutor::new(
                crate::tools::ToolRegistry::new(),
                crate::tools::PermissionManager::new(),
                tempdir.path().join("patterns.json"),
            )
            .expect("delegated say fixture: construct inert tool executor");
            let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
            let mut event_loop = super::EventLoop::new_named_brain_test_runner(
                generator,
                Vec::new(),
                Arc::new(tokio::sync::Mutex::new(executor)),
                runtime,
            );
            event_loop.output_manager.disable_stdout();
            event_loop.runner_brain = Some("home".into());
            event_loop.home_runner_lease_active = true;

            let run_id = RunId(uuid::Uuid::new_v4());
            let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
            event_loop
                .handle_event(super::ReplEvent::NamedBrainProgramRequested(
                    crate::server::RunnerProgramRequest {
                        brain: "home".into(),
                        run_id,
                        request_seq: 3,
                        language: ProgramLanguage::Lisp,
                        source: source.clone(),
                        interaction: crate::server::RunnerProgramInteraction::Interactive,
                        grant_ceiling: None,
                        control_tx: None,
                        effect_audit: None,
                        response_tx,
                    },
                ))
                .await
                .expect("delegated program must dispatch");
            // Let the spawned VM task run so the delegation settles the way
            // production does; the completion event lands on the loop channel
            // and the settle below is the handler the loop would call.
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }

            event_loop
                .finish_named_brain_program(run_id, greeting.to_string(), None)
                .await;

            let messages = event_loop.output_manager.get_messages();
            let rendered = messages
                .iter()
                .map(|message| message.format(&crate::theme::ColorScheme::default()))
                .collect::<Vec<_>>()
                .join("\n---\n");
            assert!(
                event_loop.remote_brain_run_units.get(&run_id).is_none(),
                "INVARIANT: a delegated say program projects no Brain run group; \
                 rendered=\n{rendered}"
            );
            let say_cards = messages
                .iter()
                .filter(|message| message.say_turn_view().is_some())
                .count();
            assert_eq!(
                say_cards, 1,
                "INVARIANT: the delegated program renders exactly one say card; \
                 cards={say_cards}; rendered=\n{rendered}"
            );
            let card_view = messages
                .iter()
                .find_map(|message| message.say_turn_view())
                .expect("the delegated program must carry a say card");
            assert_eq!(
                card_view.vm.status,
                SayTurnStatus::Completed,
                "INVARIANT: the settled delegated turn is a completed card; view={card_view:?}"
            );
            assert_eq!(
                card_view.vm.program.lines.join("\n"),
                source,
                "INVARIANT: the card carries the delegated program for the reveal toggle; \
                 view={card_view:?}"
            );
            assert_eq!(
                rendered.matches(&source).count(),
                1,
                "INVARIANT: the delegated program renders exactly once across the turn's \
                 units; rendered=\n{rendered}"
            );
            assert_eq!(
                card_view
                    .vm
                    .output
                    .as_ref()
                    .map(|output| output.lines.join("\n")),
                Some(greeting.to_string()),
                "INVARIANT: the completed card's output part carries the say prose; \
                 view={card_view:?}"
            );
            assert!(
                !rendered.contains("Brain run") && !rendered.contains("Interactive run"),
                "INVARIANT: no Brain run UUID row renders for the delegated say turn; \
                 rendered=\n{rendered}"
            );

            // The daemon's terminal events for the delegated run arrive after
            // the local projection registered; neither may paint a run group
            // or a duplicate row beside the card.
            let locally_rendered = event_loop.locally_rendered_runs();
            let mut result = brain_event(
                7,
                "daemon",
                BrainEventKind::Result {
                    request_seq: 3,
                    output: greeting.to_string(),
                    error: None,
                    continuation_messages: Vec::new(),
                    invocation_metadata: None,
                },
            );
            result.run_id = Some(run_id);
            assert!(
                super::project_remote_brain_live_run_event(
                    &event_loop.output_manager,
                    &mut event_loop.remote_brain_run_units,
                    &mut event_loop.local_brain_projections,
                    true,
                    &result,
                    &locally_rendered,
                    &mut event_loop.locally_say_projected_runs,
                    None,
                ),
                "INVARIANT: the delegated run's terminal Result is handled by the \
                 projection path"
            );
            assert!(
                event_loop.locally_say_projected_runs.contains(&run_id),
                "INVARIANT: the completed pure-say turn marks the run so later lifecycle \
                 events stay suppressed; say_completed={:?}",
                event_loop.locally_say_projected_runs
            );
            assert!(
                event_loop.remote_brain_run_units.get(&run_id).is_none(),
                "INVARIANT: the daemon's terminal Result paints no run group for the \
                 delegated say turn"
            );

            let mut terminal = brain_event(
                8,
                "daemon",
                BrainEventKind::RunStatusChanged {
                    run_id,
                    status: crate::brain::BrainRunStatus::Completed,
                    detail: None,
                },
            );
            terminal.run_id = Some(run_id);
            let locally_rendered = event_loop.locally_rendered_runs();
            super::project_remote_brain_live_run_event(
                &event_loop.output_manager,
                &mut event_loop.remote_brain_run_units,
                &mut event_loop.local_brain_projections,
                true,
                &terminal,
                &locally_rendered,
                &mut std::collections::HashSet::new(),
                None,
            );
            let rendered_after = event_loop
                .output_manager
                .get_messages()
                .iter()
                .map(|message| message.format(&crate::theme::ColorScheme::default()))
                .collect::<Vec<_>>()
                .join("\n---\n");
            assert!(
                event_loop.remote_brain_run_units.get(&run_id).is_none(),
                "INVARIANT: the terminal RunStatusChanged paints no run group for the \
                 delegated say turn; rendered=\n{rendered_after}"
            );
        })
        .await;
}

/// #978 regression at the run-group creation boundary: a run this frontend
/// initiated while it holds the runner lease projects no group from its
/// `RunStarted` event, while a run initiated by another attachment still does.
#[tokio::test]
async fn locally_initiated_run_does_not_project_group_from_started_event() {
    use crate::brain::{AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus};

    let output = replay_output_manager();
    let mut projections = std::collections::HashMap::new();
    let my_attachment = AttachmentId(uuid::Uuid::new_v4());
    let peer_attachment = AttachmentId(uuid::Uuid::new_v4());
    let make_run = |request_seq: u64, initiator: AttachmentId, initiated_by: &str| -> BrainRun {
        BrainRun {
            run_id: crate::brain::RunId(uuid::Uuid::new_v4()),
            kind: BrainRunKind::Interactive,
            parent_run_id: None,
            request_seq,
            initiating_attachment_id: initiator,
            initiated_by: initiated_by.into(),
            status: BrainRunStatus::Running,
            started_ms: 1,
            updated_ms: 1,
            detail: None,
        }
    };
    let local_run = make_run(1, my_attachment, "shammah");
    let peer_run = make_run(2, peer_attachment, "peer");
    let local_run_id = local_run.run_id;
    let peer_run_id = peer_run.run_id;
    let mut local_started = brain_event(2, "daemon", BrainEventKind::RunStarted { run: local_run });
    local_started.run_id = Some(local_run_id);
    assert!(
        super::project_remote_brain_run_event(
            &output,
            &mut projections,
            &local_started,
            &super::LocallyRenderedRuns::default(),
            Some(my_attachment),
        ),
        "a locally initiated run's RunStarted is handled without painting"
    );
    assert!(
        projections.get(&local_run_id).is_none(),
        "INVARIANT: the locally executed run's RunStarted paints no run group (#978); \
         projected={}",
        projections.len()
    );
    let mut peer_started = brain_event(3, "daemon", BrainEventKind::RunStarted { run: peer_run });
    peer_started.run_id = Some(peer_run_id);
    assert!(super::project_remote_brain_run_event(
        &output,
        &mut projections,
        &peer_started,
        &super::LocallyRenderedRuns::default(),
        Some(my_attachment),
    ));
    assert!(
        projections.get(&peer_run_id).is_some(),
        "INVARIANT: a peer-initiated run still projects its run group (control case); \
         projected={}",
        projections.len()
    );
}

/// #978 regression at the push boundary: the daemon echoes this frontend's
/// pushed `Program` event back over the watch; the echo must not paint a
/// second source unit beside the delegated execution's own one, whichever
/// arrives first.
#[tokio::test]
async fn pushed_program_echo_is_not_painted_twice() {
    tokio::task::LocalSet::new()
        .run_until(async {
            pushed_program_echo_is_not_painted_twice_body().await;
        })
        .await;
}

async fn pushed_program_echo_is_not_painted_twice_body() {
    use crate::brain::BrainEventKind;

    let source = "(say \"echo probe\")";
    let mut event_loop = {
        let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
        let tempdir = tempfile::tempdir().expect("echo fixture: isolated tool state");
        let executor = crate::tools::ToolExecutor::new(
            crate::tools::ToolRegistry::new(),
            crate::tools::PermissionManager::new(),
            tempdir.path().join("patterns.json"),
        )
        .expect("echo fixture: construct inert tool executor");
        let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
        let loop_ = super::EventLoop::new_named_brain_test_runner(
            generator,
            Vec::new(),
            Arc::new(tokio::sync::Mutex::new(executor)),
            runtime,
        );
        loop_.output_manager.disable_stdout();
        loop_
    };
    event_loop.record_locally_pushed_program(source.to_string());

    let mut echo = brain_event(
        1,
        &event_loop.participant_subject.clone(),
        BrainEventKind::Program {
            language: crate::brain::ProgramLanguage::Lisp,
            source: source.to_string(),
        },
    );
    echo.run_id = None;
    assert!(
        event_loop.take_locally_pushed_program_echo(&echo),
        "INVARIANT: the daemon's echo of a locally pushed program matches the marker"
    );
    assert!(
        !event_loop.take_locally_pushed_program_echo(&echo),
        "INVARIANT: the marker is consumed once — a repeated echo is a real event"
    );

    let mut peer_push = brain_event(
        2,
        "peer@elsewhere",
        BrainEventKind::Program {
            language: crate::brain::ProgramLanguage::Lisp,
            source: source.to_string(),
        },
    );
    peer_push.run_id = None;
    assert!(
        !event_loop.take_locally_pushed_program_echo(&peer_push),
        "INVARIANT: a peer's Program event with the same bytes is never treated as \
         this frontend's echo"
    );
    let messages = event_loop.output_manager.get_messages();
    assert!(
        messages.is_empty(),
        "INVARIANT: the probe helper paints nothing; messages={}",
        messages.len()
    );
}

/// #978 control regression: a genuinely remote run's events still project
/// the legacy run group with its rows even while a local projection exists
/// for a different run.
#[tokio::test]
async fn remote_run_events_still_paint_group_rows_beside_local_projections() {
    use crate::brain::{AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus};

    let greeting = "remote turn output";
    let source = "(say \"remote turn output\")";
    let output = replay_output_manager();
    let mut projections = std::collections::HashMap::new();
    let remote_run_id = crate::brain::RunId(uuid::Uuid::new_v4());
    let run = BrainRun {
        run_id: remote_run_id,
        kind: BrainRunKind::Interactive,
        parent_run_id: None,
        request_seq: 9,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "peer".into(),
        status: BrainRunStatus::Running,
        started_ms: 1,
        updated_ms: 1,
        detail: None,
    };
    let mut run_started = brain_event(1, "daemon", BrainEventKind::RunStarted { run });
    run_started.run_id = Some(remote_run_id);
    let mut program = brain_event(
        2,
        "provider",
        BrainEventKind::Program {
            language: crate::brain::ProgramLanguage::Lisp,
            source: source.to_string(),
        },
    );
    program.run_id = Some(remote_run_id);
    let mut result = brain_event(
        3,
        "daemon",
        BrainEventKind::Result {
            request_seq: 10,
            output: greeting.to_string(),
            error: None,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
    );
    result.run_id = Some(remote_run_id);
    let mut terminal = brain_event(
        4,
        "daemon",
        BrainEventKind::RunStatusChanged {
            run_id: remote_run_id,
            status: BrainRunStatus::Completed,
            detail: None,
        },
    );
    terminal.run_id = Some(remote_run_id);

    // A local projection for a DIFFERENT run stays queued in the active turn.
    let local_run_id = crate::brain::RunId(uuid::Uuid::new_v4());
    let mut local_projections = std::collections::VecDeque::from([LocalBrainProjection {
        run_id: local_run_id,
        source: "(say \"other turn\")".into(),
        output: "other turn output".into(),
        tool_ids: std::collections::HashSet::new(),
        approval_ids: std::collections::HashSet::new(),
        program_seq: None,
        transient_output_unit: None,
        failed: false,
    }]);

    for event in [&run_started, &program, &result, &terminal] {
        assert!(
            super::project_remote_brain_live_run_event(
                &output,
                &mut projections,
                &mut local_projections,
                true,
                event,
                &super::LocallyRenderedRuns::default(),
                &mut std::collections::HashSet::new(),
                None,
            ),
            "each remote run event projects through the run-group path"
        );
    }
    assert!(
        local_projections.front().is_some(),
        "INVARIANT: the remote run's events must not consume the unrelated local projection"
    );
    let projection = projections
        .get(&remote_run_id)
        .unwrap_or_else(|| panic!("INVARIANT: a remote run still projects its run group"));
    let rendered = projection
        .unit
        .format(&crate::theme::ColorScheme::default());
    for expected in ["status", "Lisp program", "result", greeting, "completed"] {
        assert!(
            rendered.contains(expected),
            "INVARIANT: a genuinely remote Brain turn still renders its legacy rows; \
             missing {expected:?}; rendered=\n{rendered}"
        );
    }
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

fn user_msg(text: &str) -> crate::providers::Message {
    crate::providers::Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
    }
}

fn assistant_msg(text: &str) -> crate::providers::Message {
    crate::providers::Message {
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
        crate::providers::Message {
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

fn make_msgs(roles: &[&str]) -> Vec<crate::providers::Message> {
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
    use crate::providers::Message;

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
    use crate::providers::Message;

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
    use crate::providers::Message;

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

fn make_tool_use(name: &str, input: serde_json::Value) -> crate::tools::ToolUse {
    crate::tools::ToolUse {
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

#[test]
fn test_tool_approval_dialog_summary_write_create_does_not_dump_content() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("docs.html");
    let html = format!("<!DOCTYPE html>{}", " <div>page content</div>".repeat(400));
    let tool = make_tool_use(
        "write",
        serde_json::json!({"file_path": path.to_string_lossy(), "content": html}),
    );
    let summary = tool_approval_summary(&tool);
    assert!(
        !summary.contains("<!DOCTYPE") && !summary.contains("page content"),
        "write approval must not dump file bytes: {summary:?}"
    );
    assert!(
        summary.contains("docs.html") && summary.contains("create"),
        "new write must name the path and say create: {summary:?}"
    );
    assert!(
        summary.contains("KB") || summary.contains("bytes") || summary.contains("MB"),
        "write approval must include a byte count: {summary:?}"
    );
}

#[test]
fn test_tool_approval_dialog_summary_write_overwrite_existing() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "old").unwrap();
    let tool = make_tool_use(
        "Write",
        serde_json::json!({
            "file_path": file.path().to_string_lossy(),
            "content": "new contents"
        }),
    );
    let summary = tool_approval_summary(&tool);
    assert!(
        summary.contains("overwrite") && summary.contains("replacing existing content"),
        "existing write must say overwrite: {summary:?}"
    );
    assert!(
        !summary.contains("new contents"),
        "overwrite summary must not dump replacement bytes: {summary:?}"
    );
}

#[test]
fn test_tool_approval_dialog_summary_edit_is_one_line() {
    let tool = make_tool_use(
        "edit",
        serde_json::json!({
            "file_path": "src/main.rs",
            "old_string": "old\n".repeat(80),
            "new_string": "new\n".repeat(80)
        }),
    );
    let summary = tool_approval_summary(&tool);
    assert_eq!(
        summary.lines().count(),
        1,
        "edit summary must stay one line: {summary:?}"
    );
    assert!(
        summary.contains("src/main.rs") && !summary.contains("old\n"),
        "edit summary must not dump the replacement: {summary:?}"
    );
}

// ── dialog_result_to_confirmation (4-option Claude Code style, #902) ──────

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
fn test_dialog_result_selected_2_approve_pattern_persistent() {
    // Option "3. Yes, and always allow bash:*" → ApprovePatternPersistent (#902).
    // The tool-execution path routes this variant through
    // `approve_pattern_persistent` + `save_patterns()`, which writes the
    // disk-backed store that survives restart.
    let tool = make_tool_use("bash", serde_json::json!({"command": "cargo fmt"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(2), &tool);
    match result {
        crate::cli::repl_event::events::ConfirmationResult::ApprovePatternPersistent(p) => {
            assert_eq!(
                p.tool_name, "bash",
                "persistent pattern tool_name should match ToolUse.name"
            );
            assert_eq!(
                p.pattern, "*",
                "persistent grant is the same wildcard shape"
            );
            assert!(
                p.description.contains("persistent"),
                "description must mark the grant persistent: {}",
                p.description
            );
        }
        other => panic!("expected ApprovePatternPersistent, got {:?}", other),
    }
}

#[test]
fn test_dialog_result_selected_3_deny() {
    // Option "4. No" → Deny
    let tool = make_tool_use("bash", serde_json::json!({"command": "rm -rf /"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(3), &tool);
    assert!(
        matches!(
            result,
            crate::cli::repl_event::events::ConfirmationResult::Deny
        ),
        "index 3 (No) should be Deny, got {:?}",
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
    // Index 3 ("4. No") → Deny; index 99 → Deny. Nothing panics.
    let tool = make_tool_use("read", serde_json::json!({"file_path": "src/lib.rs"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(3), &tool);
    assert!(
        matches!(
            result,
            crate::cli::repl_event::events::ConfirmationResult::Deny
        ),
        "index 3 is No/Deny in 4-option dialog, got {:?}",
        result
    );
}

/// Clock-free `ReplMode` fixtures. `Planning` and `Executing` carry timestamps,
/// so they are pinned to the epoch: nothing here reads a clock.
fn modes_under_test() -> Vec<(&'static str, crate::cli::repl::ReplMode)> {
    use crate::cli::repl::ReplMode;

    let at = chrono::DateTime::from_timestamp(0, 0).expect("epoch is a valid timestamp");
    let plan_path = std::path::PathBuf::from("/nonexistent/finch-438-plan.md");

    vec![
        ("Normal", ReplMode::Normal),
        ("AutoAccept", ReplMode::AutoAccept),
        (
            "Planning",
            ReplMode::Planning {
                task: "explore".to_string(),
                plan_path: plan_path.clone(),
                created_at: at,
            },
        ),
        (
            "Executing",
            ReplMode::Executing {
                task: "explore".to_string(),
                plan_path,
                approved_at: at,
            },
        ),
    ]
}

/// Host-effect tools the status bar must not claim are auto-accepted.
/// `PermissionManager::check_tool_use` does not take a `ReplMode`, so the
/// outcome is the same in every mode.
fn host_effect_tools() -> [(&'static str, serde_json::Value); 3] {
    [
        ("write", serde_json::json!({"path": "src/lib.rs"})),
        ("edit", serde_json::json!({"path": "src/lib.rs"})),
        ("bash", serde_json::json!({"command": "echo hi"})),
    ]
}

/// Labels that claim waived approval are allowed only on `AutoAccept`,
/// which actually skips the REPL approval dialog. Other modes must not
/// advertise a waiver (`PermissionManager` still returns AskUser).
#[test]
fn test_mode_indicator_never_claims_edits_are_accepted_automatically() {
    let permissions = crate::tools::PermissionManager::new();
    for (tool, input) in host_effect_tools() {
        let outcome = permissions.check_tool_use(tool, &input);
        assert!(
            matches!(outcome, crate::tools::PermissionCheck::AskUser(_)),
            "invariant: host-effect tool {tool} must ask via \
             crate::tools::PermissionManager::check_tool_use; that function \
             never takes a ReplMode. AutoAccept waives at the REPL dialog, \
             not inside check_tool_use. outcome={outcome:?} input={input}"
        );
    }

    const CLAIMS_OF_WAIVED_APPROVAL: [&str; 13] = [
        "accept edits",
        "accepts edits",
        "auto-accept",
        "auto accept",
        "autoaccept",
        "accepted automatically",
        "automatically accept",
        "no confirmation",
        "without confirmation",
        "without approval",
        "without prompts",
        "skip confirmation",
        "skips confirmation",
    ];

    for (mode_name, mode) in modes_under_test() {
        let indicator = super::plan_mode_indicator(&mode);
        let lowered = indicator.to_lowercase();
        let claims = CLAIMS_OF_WAIVED_APPROVAL
            .iter()
            .any(|claim| lowered.contains(claim));
        if mode.auto_accepts_host_effects() {
            assert!(
                claims,
                "AutoAccept must advertise that prompts are skipped: {indicator:?}"
            );
        } else {
            assert!(
                !claims,
                "mode={mode_name} must not advertise waived approvals; \
                 indicator={indicator:?}"
            );
        }
    }
}

/// Each label must state something true of its own variant, and must reach
/// the status bar as written.
///
/// Required substrings are behavioural facts, each checkable in source:
///
/// - `Normal` and `Executing` prompt for approval. Neither is exempt from
///   `check_tool_use`; `Executing` differs from `Normal` only in that a plan
///   has been approved, not in what a tool call costs the user.
/// - `Planning` is restricted to inspection tools — `ToolExecutor::execute_tool`
///   rejects anything outside read/glob/grep/web_fetch plus the plan tools.
/// - Shift+tab is live in every mode. `KeyCode::BackTab` maps to `/cycle-mode`
///   (`crates/finch-tui/src/async_input.rs`). Normal enters AutoAccept; AutoAccept
///   enters Planning; Planning/Executing return to Normal. `/plan` is a
///   separate PlanModeToggle and still enters Planning from Normal.
#[test]
fn test_mode_indicator_describes_each_modes_actual_behavior() {
    use crate::cli::status_bar::{StatusBar, StatusLineType};

    let required: [(&str, &str, &str); 4] = [
        (
            "Normal",
            "confirm",
            "Normal is subject to check_tool_use like every other mode; \
             write/edit/bash return AskUser",
        ),
        (
            "AutoAccept",
            "without prompts",
            "AutoAccept skips the REPL approval dialog for tools and VM programs",
        ),
        (
            "Planning",
            "inspection",
            "ToolExecutor::execute_tool rejects any tool outside \
             read/glob/grep/web_fetch plus the plan tools while Planning",
        ),
        (
            "Executing",
            "confirm",
            "Executing lifts the Planning tool restriction only; it does not \
             exempt anything from check_tool_use",
        ),
    ];

    for (mode_name, mode) in modes_under_test() {
        let indicator = super::plan_mode_indicator(&mode);
        let lowered = indicator.to_lowercase();

        let (_, needle, why) = required
            .iter()
            .find(|(name, _, _)| *name == mode_name)
            .unwrap_or_else(|| {
                panic!(
                    "invariant: every ReplMode variant needs a stated required \
                     fact in this test, so a new variant cannot ship an \
                     unchecked label. unmatched mode={mode_name}"
                )
            });

        assert!(
            lowered.contains(needle),
            "invariant: a REPL mode indicator must state what its mode \
             actually does. mode={mode_name} mode_value={mode:?} \
             indicator={indicator:?} missing={needle:?} grounds={why:?}"
        );

        assert!(
            !lowered.contains("disabled"),
            "invariant: no mode may report shift+tab as disabled — \
             KeyCode::BackTab maps to /cycle-mode \
             (crates/finch-tui/src/async_input.rs). \
             mode={mode_name} mode_value={mode:?} indicator={indicator:?}"
        );

        let status_bar = StatusBar::new();
        let line = StatusLineType::Custom("plan_mode".to_string());
        status_bar.update_line(line.clone(), indicator);
        let shown = status_bar.get_line(&line);

        assert_eq!(
            shown.as_deref(),
            Some(indicator),
            "invariant: the plan_mode status line must carry the mode \
             indicator verbatim, since it is the only surface reporting mode. \
             mode={mode_name} mode_value={mode:?} \
             indicator={indicator:?} status_line={shown:?}"
        );
    }
}

struct PlanningWriteProbe;

#[async_trait::async_trait]
impl crate::tools::Tool for PlanningWriteProbe {
    fn name(&self) -> &str {
        "write"
    }

    fn effect(&self) -> finch_programs::ExecutionEffect {
        finch_programs::ExecutionEffect::WorkspaceWrite
    }

    fn description(&self) -> &str {
        "probe: execute must not run in Planning"
    }

    fn input_schema(&self) -> crate::tools::ToolInputSchema {
        crate::tools::ToolInputSchema::simple(vec![("path", "unused")])
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _context: &crate::tools::ToolContext<'_>,
    ) -> anyhow::Result<String> {
        Ok("must-not-execute-in-planning".to_string())
    }
}

/// The executor's only `ReplMode` branch *restricts* `Planning`; it does not
/// waive `AskUser` for write. Grounds the Planning label in the production
/// execute_tool path rather than in the ReplMode doc comment. The probe is
/// named `write` so a missed gate fails the assertion instead of opening
/// `$EDITOR`.
#[tokio::test]
async fn test_mode_indicator_planning_executor_restricts_write_instead_of_waiving_approval() {
    use crate::cli::repl::ReplMode;
    use crate::tools::{PermissionManager, ToolExecutor, ToolRegistry, ToolUse};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(PlanningWriteProbe));
    let tempdir = tempfile::tempdir().expect("isolated tool-pattern store for planning gate");
    let executor = ToolExecutor::new(
        registry,
        PermissionManager::new(),
        tempdir.path().join("patterns.json"),
    )
    .expect("construct executor for planning-mode write gate");

    let at = chrono::DateTime::from_timestamp(0, 0).expect("epoch is a valid timestamp");
    let planning = Arc::new(RwLock::new(ReplMode::Planning {
        task: "explore".to_string(),
        plan_path: std::path::PathBuf::from("/nonexistent/finch-438-plan.md"),
        created_at: at,
    }));
    let tool_use = ToolUse::new(
        "write".to_string(),
        serde_json::json!({"path": "src/lib.rs", "content": "must not be written"}),
    );

    let result = executor
        .execute_tool(
            &tool_use,
            None::<fn() -> anyhow::Result<()>>,
            Some(planning), // repl_mode
            None,           // plan_content
            None,           // live_output
            None,           // effect_audit
        )
        .await
        .expect("planning restriction returns ToolResult, not a transport error");

    assert!(
        result.is_error,
        "invariant: ToolExecutor::execute_tool must reject write in Planning \
         rather than auto-accepting it. content={:?} is_error={}",
        result.content, result.is_error
    );
    assert!(
        result.content.contains("not allowed in planning mode"),
        "invariant: the Planning executor branch must name the mode restriction, \
         not a permission waiver. content={:?}",
        result.content
    );
    assert!(
        !result.content.contains("must-not-execute-in-planning"),
        "invariant: Planning must return before execute, so the write probe \
         body never runs. content={:?}",
        result.content
    );
}

// --- Session-cumulative token accounting (status line + per-Brain ledger) ---

fn session_usage_test_event_loop() -> (super::EventLoop, tempfile::TempDir) {
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    let state_dir =
        tempfile::tempdir().expect("session-usage fixture: create isolated tool state directory");
    let executor = crate::tools::ToolExecutor::new(
        crate::tools::ToolRegistry::new(),
        crate::tools::PermissionManager::new(),
        state_dir.path().join("patterns.json"),
    )
    .expect("session-usage fixture: construct inert tool executor");
    let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
    let mut event_loop = super::EventLoop::new_named_brain_test_runner(
        generator,
        Vec::new(),
        Arc::new(tokio::sync::Mutex::new(executor)),
        Arc::clone(&runtime),
    );
    // The per-Brain checkpoint must live in this test's own directory, never
    // in the developer's real ~/.finch.
    let usage_dir = tempfile::tempdir()
        .expect("session-usage fixture: create isolated usage checkpoint directory");
    event_loop.reset_session_usage_for_tests(Some(usage_dir.path().join("usage.json")));
    (event_loop, usage_dir)
}

fn stats_update_event(model: &str, input: u32, output: u32) -> super::ReplEvent {
    super::ReplEvent::StatsUpdate {
        model: model.to_string(),
        input_tokens: Some(input),
        output_tokens: Some(output),
        latency_ms: Some(10),
        primary_allowance_used_percent: None,
        secondary_allowance_used_percent: None,
    }
}

#[tokio::test]
async fn test_stats_update_accumulates_session_usage_into_status_line_and_persists() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, usage_dir) = session_usage_test_event_loop();

            event_loop
                .handle_event(stats_update_event("claude-sonnet-4-6", 1500, 300))
                .await
                .expect("first usage dispatch must succeed");
            event_loop
                .handle_event(stats_update_event("qwen-local", 2000, 500))
                .await
                .expect("second usage dispatch must succeed");

            let ledger = &event_loop.session_usage;
            assert_eq!(
                (ledger.input_tokens, ledger.output_tokens, ledger.turns),
                (3500, 800, 2),
                "the ledger must accumulate every provider-reported turn exactly once; ledger={ledger:?}"
            );

            let line = event_loop
                .status_bar
                .get_line(&crate::cli::status_bar::StatusLineType::SessionUsage)
                .expect("dispatching usage must surface the session readout in the status line");
            assert_eq!(
                line, "this session: 3.5k in / 800 out",
                "the status line must show the cumulative tokens in the issue's format; line={line:?}"
            );
            assert!(
                !line.contains('$'),
                "no price source exists yet, so the readout must stay tokens-only; line={line:?}"
            );

            let checkpoint = usage_dir.path().join("usage.json");
            let restored = crate::cli::usage::SessionUsageLedger::load(&checkpoint)
                .expect("the checkpoint must be readable")
                .expect("every recorded turn must reach the per-Brain checkpoint");
            assert_eq!(
                restored,
                event_loop.session_usage,
                "attach/resume must restore the exact running total; ledger={:?} checkpoint={restored:?}",
                event_loop.session_usage
            );
        })
        .await;
}

#[tokio::test]
async fn test_usage_reset_command_clears_session_totals_and_checkpoint() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, usage_dir) = session_usage_test_event_loop();

            event_loop
                .handle_event(stats_update_event("claude-sonnet-4-6", 1500, 300))
                .await
                .expect("usage dispatch must succeed before reset");

            event_loop
                .handle_user_input("/usage reset".to_string())
                .await
                .expect("the explicit reset command must dispatch");

            let ledger = &event_loop.session_usage;
            assert!(
                ledger.is_empty(),
                "/usage reset is the only path back to zero and must clear every total; ledger={ledger:?}"
            );
            assert_eq!(
                event_loop
                    .status_bar
                    .get_line(&crate::cli::status_bar::StatusLineType::SessionUsage),
                None,
                "a zeroed ledger must leave the status strip instead of showing a fake zero burn"
            );

            let checkpoint = usage_dir.path().join("usage.json");
            let restored = crate::cli::usage::SessionUsageLedger::load(&checkpoint)
                .expect("the reset checkpoint must be readable")
                .expect("reset must persist, or attach/resume would resurrect the old total");
            assert!(
                restored.is_empty(),
                "the checkpoint must reflect the reset state; checkpoint={restored:?}"
            );

            // /usage after reset reports the empty state instead of zero rows.
            event_loop
                .handle_user_input("/usage".to_string())
                .await
                .expect("the display command must dispatch");
            let messages: Vec<String> = event_loop
                .output_manager
                .get_messages()
                .iter()
                .map(|message| message.format(&crate::theme::ColorScheme::default()))
                .collect();
            assert!(
                messages
                    .iter()
                    .any(|message| message.contains("No usage recorded this session yet.")),
                "/usage must report the empty state; messages={messages:?}"
            );
            assert!(
                messages
                    .iter()
                    .any(|message| message.contains("Session usage reset.")),
                "the reset confirmation must reach the scrollback; messages={messages:?}"
            );
        })
        .await;
}

type ObservedLlmQuery = (Uuid, String, Vec<crate::providers::Message>);

fn observe_llm_queries(
    event_loop: &mut EventLoop,
) -> tokio::sync::mpsc::UnboundedReceiver<ObservedLlmQuery> {
    let conversation = Arc::clone(&event_loop.conversation);
    let mut llm_rx = event_loop
        .llm_rx
        .take()
        .expect("test fixture must observe LlmRequest before the worker starts");
    let (observed_tx, observed_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(LlmRequest::Query {
            id,
            text,
            admission,
            admission_ready,
            spawned,
            publication,
            ..
        }) = llm_rx.recv().await
        {
            if let Some(ready) = admission_ready {
                let _ = ready.send(());
            }
            if let Some(admission) = admission {
                if admission.await.is_err() {
                    continue;
                }
            }
            // Snapshot after admission so a tool-round continuation has already
            // committed results (and any injected user text) before generate.
            let snapshot = conversation.read().await.get_messages();
            if let Some(spawned) = spawned {
                let _ = spawned.send(());
            }
            if let Some(publication) = publication {
                if publication.await.is_err() {
                    continue;
                }
            }
            let _ = observed_tx.send((id, text, snapshot));
        }
    });
    observed_rx
}

async fn start_executing_tools_query(
    event_loop: &mut EventLoop,
    tool_id: &str,
) -> (Uuid, crate::cli::conversation::ToolRoundToken) {
    let query_id = event_loop.query_states.create_query(Vec::new()).await;
    assert!(
        event_loop
            .query_states
            .begin_tool_execution(query_id, 1)
            .await,
        "the in-flight query must be ExecutingTools so finalize_tool_execution runs"
    );
    *event_loop.active_query_id.write().await = Some(query_id);
    event_loop
        .conversation
        .write()
        .await
        .add_user_message("original turn".to_string());
    let round_token = event_loop
        .conversation
        .write()
        .await
        .stage_assistant(
            query_id,
            crate::providers::Message {
                role: "assistant".into(),
                content: vec![crate::providers::ContentBlock::ToolUse {
                    id: tool_id.into(),
                    name: "Read".into(),
                    input: serde_json::json!({"path": "README.md"}),
                }],
            },
        )
        .expect("stage the tool round whose completing result finalizes");
    let work_unit = event_loop.output_manager.start_work_unit("Tools");
    let row_idx = work_unit.add_row("Read(README.md)");
    event_loop.active_tool_uses.write().await.insert(
        tool_id.to_string(),
        (
            "Read".into(),
            serde_json::json!({"path": "README.md"}),
            Arc::clone(&work_unit),
            row_idx,
        ),
    );
    (query_id, round_token)
}

fn user_text_messages(messages: &[crate::providers::Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == "user")
        .filter_map(|message| {
            let text = message.text_content();
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        })
        .collect()
}

fn assert_no_consecutive_user_roles(messages: &[crate::providers::Message], detail: &str) {
    for window in messages.windows(2) {
        assert_ne!(
            (window[0].role.as_str(), window[1].role.as_str()),
            ("user", "user"),
            "{detail}; consecutive user roles would Claude 400 / hang; messages={messages:?}"
        );
    }
}

/// Queued user text submitted during ExecutingTools must appear in conversation
/// after the current tool-round results and before the next provider generate.
/// It must not start a second `execute_query_inner` / `active_query_id`.
#[tokio::test]
async fn test_pending_user_message_injects_before_next_tool_round_generate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let mut observed_rx = observe_llm_queries(&mut event_loop);
            let tool_id = "call_inject_round";
            let (query_id, round_token) =
                start_executing_tools_query(&mut event_loop, tool_id).await;

            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: "steer now".to_string(),
                })
                .await
                .expect("queuing a user turn during ExecutingTools must succeed");
            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: "and also this".to_string(),
                })
                .await
                .expect("a second queued turn must preserve order");
            assert_eq!(
                event_loop
                    .pending_queries
                    .iter()
                    .map(|(text, _, _)| text.as_str())
                    .collect::<Vec<_>>(),
                ["steer now", "and also this"],
                "both turns must sit on pending_queries until the tool-round boundary; queued={:?}",
                event_loop.pending_queries
            );
            assert_eq!(
                *event_loop.active_query_id.read().await,
                Some(query_id),
                "queuing must not start a second active_query_id"
            );

            event_loop
                .handle_event(ReplEvent::ToolResult {
                    query_id,
                    round_token,
                    tool_id: tool_id.to_string(),
                    result: Ok("readme contents".to_string()),
                })
                .await
                .expect("completing the tool round must finalize");

            assert!(
                event_loop.pending_queries.is_empty(),
                "the tool-round boundary must drain pending_queries so StreamingComplete cannot start them as a new query; queued={:?}",
                event_loop.pending_queries
            );
            assert_eq!(
                *event_loop.active_query_id.read().await,
                Some(query_id),
                "inject must keep the in-flight query; a second execute_query_inner would replace active_query_id"
            );

            let live = event_loop.conversation.read().await.get_messages();
            assert_no_consecutive_user_roles(
                &live,
                "tool-round inject must not add a second user message",
            );
            let last = live
                .last()
                .expect("conversation must include the injected turn");
            assert_eq!(
                last.role, "user",
                "the last provider-visible message before the next generate must be the tool-result user turn; last={last:?}"
            );
            assert!(
                matches!(
                    last.content.as_slice(),
                    [
                        crate::providers::ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error: None
                        },
                        crate::providers::ContentBlock::Text { text: first },
                        crate::providers::ContentBlock::Text { text: second },
                    ] if tool_use_id == tool_id
                        && content == "readme contents"
                        && first == "steer now"
                        && second == "and also this"
                ),
                "queued text must be ContentBlock::Text on the same user message as the ToolResult; last={last:?}"
            );

            let observed = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                observed_rx.recv(),
            )
            .await
            .expect("the completing tool round must send a continuation LlmRequest::Query")
            .expect("the LLM request channel must stay open for the continuation");
            assert_eq!(
                observed.0, query_id,
                "continuation must reuse the in-flight query id, not start a second query; observed={observed:?}"
            );
            assert_eq!(
                observed.1, "",
                "tool-round continuation Query text is empty; the injected user text lives in conversation; observed={observed:?}"
            );
            assert_no_consecutive_user_roles(
                &observed.2,
                "continuation snapshot must not contain consecutive user roles",
            );
            let observed_last = observed
                .2
                .last()
                .expect("continuation snapshot must include the tool-result user turn");
            assert!(
                matches!(
                    observed_last.content.as_slice(),
                    [
                        crate::providers::ContentBlock::ToolResult { .. },
                        crate::providers::ContentBlock::Text { text: first },
                        crate::providers::ContentBlock::Text { text: second },
                    ] if first == "steer now" && second == "and also this"
                ),
                "the continuation snapshot taken after admission must already contain queued text on the tool-result user message; last={observed_last:?}"
            );

            event_loop
                .handle_event(ReplEvent::StreamingComplete {
                    query_id,
                    full_response: "done".to_string(),
                })
                .await
                .expect("terminal StreamingComplete must dispatch");
            assert!(
                observed_rx.try_recv().is_err(),
                "StreamingComplete must not re-start injected strings as a new query; a second LlmRequest would appear here"
            );
        })
        .await;
}

/// Plan-approval resets history to one execution directive. Queued steering
/// folds into that same user message rather than a second user turn.
#[tokio::test]
async fn test_pending_user_message_folds_into_plan_approval_directive() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let mut observed_rx = observe_llm_queries(&mut event_loop);
            let tool_id = "call_present_plan";
            let (query_id, round_token) =
                start_executing_tools_query(&mut event_loop, tool_id).await;
            let at = chrono::DateTime::from_timestamp(0, 0).expect("epoch is a valid timestamp");
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::Executing {
                task: "implement".to_string(),
                plan_path: std::path::PathBuf::from("/nonexistent/finch-814-plan.md"),
                approved_at: at,
            };

            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: "do the tests first".to_string(),
                })
                .await
                .expect("queuing during plan execution must succeed");

            let directive =
                "Plan approved by user. Execute this plan step by step:\n\n1. write tests";
            event_loop
                .handle_event(ReplEvent::ToolResult {
                    query_id,
                    round_token,
                    tool_id: tool_id.to_string(),
                    result: Ok(directive.to_string()),
                })
                .await
                .expect("plan-approval finalize must dispatch");

            assert!(
                event_loop.pending_queries.is_empty(),
                "plan-approval must drain pending_queries; queued={:?}",
                event_loop.pending_queries
            );
            assert_eq!(
                *event_loop.active_query_id.read().await,
                Some(query_id),
                "plan continuation must reuse the in-flight query"
            );

            let live = event_loop.conversation.read().await.get_messages();
            assert_eq!(
                live.len(),
                1,
                "plan-approval must reset to a single user message; messages={live:?}"
            );
            assert_no_consecutive_user_roles(
                &live,
                "plan-approval must not append a second user message after the directive",
            );
            assert_eq!(live[0].role, "user");
            assert!(
                matches!(
                    live[0].content.as_slice(),
                    [
                        crate::providers::ContentBlock::Text { text: first },
                        crate::providers::ContentBlock::Text { text: second },
                    ] if first == directive && second == "do the tests first"
                ),
                "queued steering must fold into the one directive user message; last={:?}",
                live[0]
            );

            let observed =
                tokio::time::timeout(std::time::Duration::from_secs(3), observed_rx.recv())
                    .await
                    .expect("plan continuation must send LlmRequest::Query")
                    .expect("the LLM request channel must stay open");
            assert_eq!(observed.0, query_id);
            assert_eq!(observed.1, "");
            assert_eq!(observed.2.len(), 1);
            assert_no_consecutive_user_roles(&observed.2, "plan continuation snapshot");
        })
        .await;
}

/// Continuation publication failure must restore live history and put the
/// drained queue back in FIFO order so QueryFailed can start those turns.
#[tokio::test]
async fn test_pending_user_messages_restored_in_order_when_continuation_fails() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let conversation = Arc::clone(&event_loop.conversation);
            let mut llm_rx = event_loop
                .llm_rx
                .take()
                .expect("test fixture must observe LlmRequest before the worker starts");
            tokio::spawn(async move {
                if let Some(LlmRequest::Query {
                    admission,
                    admission_ready,
                    spawned,
                    publication,
                    ..
                }) = llm_rx.recv().await
                {
                    let _ = admission_ready.unwrap().send(());
                    let _ = admission.unwrap().await;
                    let _ = spawned.unwrap().send(());
                    drop(publication);
                    drop(llm_rx);
                }
            });
            let tool_id = "call_restore_fifo";
            let (query_id, round_token) =
                start_executing_tools_query(&mut event_loop, tool_id).await;
            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: "steer now".to_string(),
                })
                .await
                .expect("queue first");
            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: "and also this".to_string(),
                })
                .await
                .expect("queue second");

            event_loop
                .handle_event(ReplEvent::ToolResult {
                    query_id,
                    round_token,
                    tool_id: tool_id.to_string(),
                    result: Ok("readme contents".to_string()),
                })
                .await
                .expect("completing the tool round must attempt continuation");

            assert_eq!(
                event_loop
                    .pending_queries
                    .iter()
                    .map(|(text, _, _)| text.as_str())
                    .collect::<Vec<_>>(),
                ["steer now", "and also this"],
                "publication failure must restore pending_queries in FIFO order; queued={:?}",
                event_loop.pending_queries
            );
            let live = conversation.read().await.get_messages();
            assert_eq!(
                live.len(),
                1,
                "publication failure must roll history back to pre-inject; live={live:?}"
            );
            assert_eq!(live[0].text_content(), "original turn");
            assert_no_consecutive_user_roles(&live, "rolled-back history");
            assert!(
                live.iter()
                    .all(|message| !message.text_content().contains("steer now")),
                "queued text must not remain in live history after rollback; live={live:?}"
            );

            let mut failed = None;
            while let Ok(event) = event_loop.event_rx.try_recv() {
                if matches!(
                    event,
                    ReplEvent::QueryFailed {
                        query_id: id,
                        ..
                    } if id == query_id
                ) {
                    failed = Some(event);
                }
            }
            let failed = failed.expect("finalize must emit QueryFailed after continuation failure");
            event_loop
                .handle_event(failed)
                .await
                .expect("QueryFailed must drain the restored queue");
            assert_eq!(
                event_loop
                    .pending_queries
                    .iter()
                    .map(|(text, _, _)| text.as_str())
                    .collect::<Vec<_>>(),
                ["and also this"],
                "QueryFailed must start the first restored turn and leave the rest queued; queued={:?}",
                event_loop.pending_queries
            );
            let after_fail = event_loop.conversation.read().await.get_messages();
            assert!(
                user_text_messages(&after_fail)
                    .iter()
                    .any(|text| text == "steer now"),
                "the first restored turn must start as its own query; messages={after_fail:?}"
            );
        })
        .await;
}

/// A query that never executes tools still starts queued input on
/// StreamingComplete, the drain that existed before tool-round inject.
#[tokio::test]
async fn test_pending_user_message_without_tools_drains_on_streaming_complete() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let mut observed_rx = observe_llm_queries(&mut event_loop);

            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: "first turn".to_string(),
                })
                .await
                .expect("the first user turn must start a query");
            let first_id = event_loop
                .active_query_id
                .read()
                .await
                .expect("the no-tools query must own active_query_id");
            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: "queued no-tools".to_string(),
                })
                .await
                .expect("a second turn during Processing must queue");
            assert_eq!(
                event_loop
                    .pending_queries
                    .iter()
                    .map(|(text, _, _)| text.as_str())
                    .collect::<Vec<_>>(),
                ["queued no-tools"],
                "without a tool round the queue must wait for StreamingComplete; queued={:?}",
                event_loop.pending_queries
            );

            let first = tokio::time::timeout(std::time::Duration::from_secs(3), observed_rx.recv())
                .await
                .expect("the first turn must dispatch LlmRequest::Query")
                .expect("the LLM request channel must stay open");
            assert_eq!(first.0, first_id);
            assert_eq!(first.1, "first turn");

            event_loop
                .handle_event(ReplEvent::StreamingComplete {
                    query_id: first_id,
                    full_response: "first reply".to_string(),
                })
                .await
                .expect("no-tools StreamingComplete must drain pending_queries");

            assert!(
                event_loop.pending_queries.is_empty(),
                "StreamingComplete must pop the queued no-tools turn; queued={:?}",
                event_loop.pending_queries
            );
            let second_id = event_loop
                .active_query_id
                .read()
                .await
                .expect("draining pending on StreamingComplete must start the queued turn");
            assert_ne!(
                second_id, first_id,
                "the queued no-tools turn is a new query, not an inject into the finished one"
            );
            let second =
                tokio::time::timeout(std::time::Duration::from_secs(3), observed_rx.recv())
                    .await
                    .expect("StreamingComplete must start the queued turn as LlmRequest::Query")
                    .expect("the LLM request channel must stay open for the drained turn");
            assert_eq!(second.0, second_id);
            assert_eq!(
                second.1, "queued no-tools",
                "the drained turn must be a new Query with the queued text; observed={second:?}"
            );
            assert!(
                user_text_messages(&second.2)
                    .iter()
                    .any(|text| text == "queued no-tools"),
                "the drained turn must be in conversation; snapshot={:?}",
                second.2
            );
        })
        .await;
}

/// Cancel must not leave queued text to re-fire after a later turn completes
/// (#463: queued turn must not execute out of order after cancel).
#[tokio::test]
async fn test_cancel_does_not_refire_queued_turn_out_of_order() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let mut observed_rx = observe_llm_queries(&mut event_loop);
            let tool_id = "call_cancel_queued";
            let (query_id, _round_token) =
                start_executing_tools_query(&mut event_loop, tool_id).await;

            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: "queued during tools".to_string(),
                })
                .await
                .expect("queuing during ExecutingTools must succeed");
            assert_eq!(
                event_loop.pending_queries.len(),
                1,
                "the cancel fixture must have a queued turn; queued={:?}",
                event_loop.pending_queries
            );

            event_loop
                .handle_event(ReplEvent::CancelQuery)
                .await
                .expect("CancelQuery must dispatch");
            assert!(
                event_loop.pending_queries.is_empty(),
                "CancelQuery must take pending_queries so a later StreamingComplete cannot start the queued turn out of order; queued={:?}",
                event_loop.pending_queries
            );
            assert_eq!(
                *event_loop.active_query_id.read().await,
                None,
                "cancel must clear the in-flight query"
            );

            event_loop
                .handle_event(ReplEvent::StreamingComplete {
                    query_id,
                    full_response: "late cancelled prose".to_string(),
                })
                .await
                .expect("late StreamingComplete for the cancelled query must be discarded");
            assert!(
                observed_rx.try_recv().is_err(),
                "a cancelled query must not dispatch the queued turn; observed a Query after cancel"
            );

            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: "later turn".to_string(),
                })
                .await
                .expect("a subsequent turn after cancel must start");
            let later_id = event_loop
                .active_query_id
                .read()
                .await
                .expect("the subsequent turn must own active_query_id");
            let later = tokio::time::timeout(std::time::Duration::from_secs(3), observed_rx.recv())
                .await
                .expect("the subsequent turn must dispatch LlmRequest::Query")
                .expect("the LLM request channel must stay open");
            assert_eq!(later.0, later_id);
            assert_eq!(
                later.1, "later turn",
                "the first Query after cancel must be the subsequent user turn, not the pre-cancel queue; observed={later:?}"
            );

            event_loop
                .handle_event(ReplEvent::StreamingComplete {
                    query_id: later_id,
                    full_response: "later reply".to_string(),
                })
                .await
                .expect("the subsequent turn must complete");
            assert!(
                observed_rx.try_recv().is_err(),
                "the pre-cancel queued turn must not re-fire after a later StreamingComplete; that is the queued-turn-out-of-order cancel bug"
            );
            assert!(
                !user_text_messages(&event_loop.conversation.read().await.get_messages())
                    .iter()
                    .any(|text| text == "queued during tools"),
                "cancel discards queued text rather than attaching it to a later turn; messages={:?}",
                event_loop.conversation.read().await.get_messages()
            );
        })
        .await;
}

fn completed_execution_outcome(output: &str) -> crate::runtime::ExecutionOutcome {
    crate::runtime::ExecutionOutcome {
        execution_id: uuid::Uuid::new_v4(),
        status: crate::runtime::ExecutionStatus::Completed,
        values: Vec::new(),
        output: output.to_string(),
        output_chunks: Vec::new(),
        side_effects: Vec::new(),
        vm_side_effects: Vec::new(),
        effect_journal: Vec::new(),
        diagnostics: Vec::new(),
        vm_diagnostics: Vec::new(),
        inferred_capabilities: Vec::new(),
        required_capabilities: Vec::new(),
        approval_prompts: Vec::new(),
        input_revision: 0,
        output_revision: 0,
        effect: finch_programs::ExecutionEffect::Pure,
        backend: crate::runtime::ExecutionBackend::TypedVm,
        elapsed_ms: 0,
    }
}

fn projected_work_unit(
    unit: &std::sync::Arc<crate::cli::messages::WorkUnit>,
) -> crate::cli::test_projection::TranscriptNode {
    crate::cli::test_projection::try_project_for_test(
        unit.as_ref(),
        &crate::theme::ColorScheme::default(),
    )
    .expect("projected row")
}

#[tokio::test]
async fn typed_program_complete_presents_successful_say_as_prose() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
            let tempdir = tempfile::tempdir().expect("typed-program fixture: isolated tool state");
            let executor = crate::tools::ToolExecutor::new(
                crate::tools::ToolRegistry::new(),
                crate::tools::PermissionManager::new(),
                tempdir.path().join("patterns.json"),
            )
            .expect("typed-program fixture: construct inert tool executor");
            let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
            let mut event_loop = super::EventLoop::new_named_brain_test_runner(
                generator,
                Vec::new(),
                Arc::new(tokio::sync::Mutex::new(executor)),
                Arc::clone(&runtime),
            );
            event_loop.output_manager.disable_stdout();

            let output_unit = event_loop
                .output_manager
                .start_work_unit("VM program output");
            output_unit.set_program_output();
            output_unit.append_response("Hello");
            event_loop
                .handle_event(super::ReplEvent::TypedProgramComplete {
                    output_unit: Arc::clone(&output_unit),
                    result: Ok(completed_execution_outcome("Hello")),
                })
                .await
                .expect("successful typed-program completion must dispatch");

            let row = projected_work_unit(&output_unit);
            assert_eq!(
                row.label, "\u{23fa}",
                "invariant: TypedProgramComplete success of untitled say is assistant prose; row={row:?}"
            );
            assert!(
                !row.label.contains("Program output") && !row.label.contains("Assistant response"),
                "invariant: reverting the success-arm present_as_assistant_prose call must fail this test; row={row:?}"
            );
            assert_eq!(
                row.body,
                vec!["Hello".to_string()],
                "invariant: say bytes remain the row body; row={row:?}"
            );
        })
        .await;
}

/// Production-boundary regression for #820 and #978: after a delegated
/// named-Brain `(say …)` completes, no Brain run line exists at all — the
/// run group is never projected for a locally executed turn, so a `running`
/// residue is impossible by construction and the say card carries the turn.
#[tokio::test]
async fn named_brain_say_complete_does_not_leave_run_status_running() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
            let tempdir =
                tempfile::tempdir().expect("named-brain say fixture: isolated tool state");
            let executor = crate::tools::ToolExecutor::new(
                crate::tools::ToolRegistry::new(),
                crate::tools::PermissionManager::new(),
                tempdir.path().join("patterns.json"),
            )
            .expect("named-brain say fixture: construct inert tool executor");
            let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
            let mut event_loop = super::EventLoop::new_named_brain_test_runner(
                generator,
                Vec::new(),
                Arc::new(tokio::sync::Mutex::new(executor)),
                Arc::clone(&runtime),
            );
            event_loop.output_manager.disable_stdout();
            event_loop.runner_brain = Some("home".into());
            event_loop.home_runner_lease_active = true;

            let run_id = crate::brain::RunId(Uuid::new_v4());
            let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
            event_loop
                .handle_event(super::ReplEvent::NamedBrainTurnRequested(
                    crate::server::RunnerTurnRequest {
                        brain: "home".into(),
                        run_id,
                        request_seq: 1,
                        prompt: "hello".into(),
                        context: vec![crate::providers::Message::user("hello")],
                        approval_audience: crate::brain::BrainApprovalAudience {
                            brain_id: crate::brain::BrainId(Uuid::new_v4()),
                            brain: "home".into(),
                            attachment_id: crate::brain::AttachmentId(Uuid::new_v4()),
                            subject: "runner".into(),
                            role: crate::brain::AttachmentRole::Runner,
                            environment_generation: 1,
                        },
                        approval_connection_id: None,
                        approval_tx: None,
                        effect_audit: None,
                        response_tx,
                    },
                ))
                .await
                .expect("named-Brain turn must dispatch");

            assert!(
                event_loop.remote_brain_run_units.get(&run_id).is_none(),
                "invariant: a delegated named-Brain turn projects no Brain run group (#978); \
                 its say turn renders through the local units only"
            );
            let locally_rendered = event_loop.locally_rendered_runs();
            assert!(
                locally_rendered.in_flight.contains(&run_id),
                "invariant: the delegated turn is in flight locally, so its daemon run \
                 events are suppressed; in_flight={:?} say_completed={:?}",
                locally_rendered.in_flight,
                locally_rendered.say_completed
            );

            let output_unit = event_loop
                .output_manager
                .start_work_unit("VM program output");
            output_unit.set_program_output();
            output_unit.begin_say_turn("lisp", "(say \"Hello\")");
            output_unit.append_response("Hello");
            event_loop
                .handle_event(super::ReplEvent::TypedProgramComplete {
                    output_unit: Arc::clone(&output_unit),
                    result: Ok(completed_execution_outcome("Hello")),
                })
                .await
                .expect("successful typed-program completion must dispatch");

            assert!(
                event_loop.remote_brain_run_units.get(&run_id).is_none(),
                "invariant: the delegated turn still projects no Brain run group after \
                 successful say; ids={:?}",
                event_loop
                    .output_manager
                    .get_messages()
                    .iter()
                    .map(|message| message.id())
                    .collect::<Vec<_>>()
            );
            let messages = event_loop.output_manager.get_messages();
            let say_cards = messages
                .iter()
                .filter(|message| message.say_turn_view().is_some())
                .count();
            assert_eq!(
                say_cards,
                1,
                "invariant: exactly one say card carries the delegated turn's output; \
                 cards={say_cards}; messages={}",
                messages.len()
            );
            assert!(
                messages
                    .iter()
                    .all(|message| message.say_turn_view().is_none_or(|view| {
                        view.vm.status != crate::cli::messages::SayTurnStatus::Running
                    })),
                "invariant: no card keeps wearing `running` (#820-class residue); \
                 messages={}",
                messages.len()
            );
        })
        .await;
}

#[tokio::test]
async fn typed_program_complete_keeps_failures_as_program_output() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
            let tempdir =
                tempfile::tempdir().expect("typed-program failure fixture: isolated tool state");
            let executor = crate::tools::ToolExecutor::new(
                crate::tools::ToolRegistry::new(),
                crate::tools::PermissionManager::new(),
                tempdir.path().join("patterns.json"),
            )
            .expect("typed-program failure fixture: construct inert tool executor");
            let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
            let mut event_loop = super::EventLoop::new_named_brain_test_runner(
                generator,
                Vec::new(),
                Arc::new(tokio::sync::Mutex::new(executor)),
                Arc::clone(&runtime),
            );
            event_loop.output_manager.disable_stdout();

            let output_unit = event_loop
                .output_manager
                .start_work_unit("VM program output");
            output_unit.set_program_output();
            output_unit.append_response("visible first\n");
            event_loop
                .handle_event(super::ReplEvent::TypedProgramComplete {
                    output_unit: Arc::clone(&output_unit),
                    result: Err("type error".to_string()),
                })
                .await
                .expect("failed typed-program completion must dispatch");

            let row = projected_work_unit(&output_unit);
            assert_eq!(
                row.label, "Program output",
                "invariant: TypedProgramComplete Err stays ordinary Program output; row={row:?}"
            );
            assert!(
                row.default_open,
                "invariant: failures remain expanded; row={row:?}"
            );
            assert_eq!(
                row.body,
                vec![
                    "visible first".to_string(),
                    "VM error: type error".to_string()
                ],
                "invariant: emitted prefix then diagnostic stay on the row in order; row={row:?}"
            );
            assert!(
                !row.label.contains('\u{23fa}'),
                "invariant: a failure must not wear the completed-prose glyph; row={row:?}"
            );
        })
        .await;
}

fn file_read_approval_prompt() -> crate::vm::ApprovalPrompt {
    crate::vm::ApprovalPrompt::for_request(crate::vm::CapabilityRequest {
        id: uuid::Uuid::nil(),
        execution_id: uuid::Uuid::nil(),
        effect_sequence: None,
        requirement: crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("Cargo.toml").expect("Cargo.toml is a valid selector"),
        ),
        arguments: Vec::new(),
        reason: "auto-accept program capability".to_string(),
        origin: crate::vm::SourceOrigin::generated("auto-accept-test"),
        agent_ancestry: Vec::new(),
        program_hash: "auto-accept-test".to_string(),
    })
}

fn auto_accept_event_loop() -> (super::EventLoop, tempfile::TempDir) {
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    let tempdir = tempfile::tempdir().expect("isolated tool state for auto-accept");
    let executor = crate::tools::ToolExecutor::new(
        crate::tools::ToolRegistry::new(),
        crate::tools::PermissionManager::new(),
        tempdir.path().join("patterns.json"),
    )
    .expect("construct inert tool executor");
    let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
    let event_loop = super::EventLoop::new_named_brain_test_runner(
        generator,
        Vec::new(),
        Arc::new(tokio::sync::Mutex::new(executor)),
        Arc::clone(&runtime),
    );
    (event_loop, tempdir)
}

/// Auto-accept must answer a VM/program capability prompt with AllowSession
/// and must not open the capability dialog. That is the production boundary
/// that was still prompting for every Forth/Lisp program.
#[tokio::test]
async fn test_auto_accept_allows_vm_capability_without_dialog() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::AutoAccept;
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            event_loop
                .handle_vm_approval_request(file_read_approval_prompt(), response_tx)
                .await
                .expect("auto-accept VM approval must succeed");
            let choice = response_rx
                .await
                .expect("auto-accept must send AllowOnce without a dialog");
            assert_eq!(
                choice,
                crate::vm::ApprovalChoice::AllowOnce,
                "AutoAccept must grant once so leaving the mode does not keep the capability; choice={choice:?}"
            );
            assert!(
                event_loop.pending_vm_approval.is_none(),
                "AutoAccept must not retain a pending VM dialog"
            );
            let tui = event_loop.tui_renderer.lock().await;
            assert!(
                tui.active_dialog.is_none(),
                "AutoAccept must not open the VM capability dialog; dialog={:?}",
                tui.active_dialog
            );
        })
        .await;
}

/// Normal mode still presents the VM capability dialog. Pins that AutoAccept
/// is the thing that skips, not a silent change to every mode.
#[tokio::test]
async fn test_normal_mode_still_opens_vm_capability_dialog() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
            event_loop
                .handle_vm_approval_request(file_read_approval_prompt(), response_tx)
                .await
                .expect("normal VM approval dialog must present");
            assert!(
                event_loop.pending_vm_approval.is_some(),
                "Normal must retain the pending VM dialog"
            );
            let tui = event_loop.tui_renderer.lock().await;
            assert!(
                tui.active_dialog.is_some(),
                "Normal must open the VM capability dialog"
            );
        })
        .await;
}

/// Cancelling a query with Ctrl+C must keep AutoAccept so dogfood does not
/// fall back to per-tool prompts mid-session.
#[tokio::test]
async fn test_ctrl_c_during_query_preserves_auto_accept() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::AutoAccept;
            let query_id = event_loop.query_states.create_query(Vec::new()).await;
            *event_loop.active_query_id.write().await = Some(query_id);
            event_loop
                .handle_event(super::ReplEvent::CancelQuery)
                .await
                .expect("query cancel must dispatch");
            let mode = event_loop.mode.read().await.clone();
            assert!(
                mode.auto_accepts_host_effects(),
                "Ctrl+C on a query must keep AutoAccept; mode={mode:?}"
            );
        })
        .await;
}

/// Idle Ctrl+C in AutoAccept exits Finch, like Normal. It must not be treated
/// as a plan overlay that silently drops back to confirmation mode.
#[tokio::test]
async fn test_idle_ctrl_c_in_auto_accept_exits_finch() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::AutoAccept;
            event_loop
                .handle_event(super::ReplEvent::CancelQuery)
                .await
                .expect("idle cancel must dispatch");
            let mode = event_loop.mode.read().await.clone();
            assert!(
                mode.auto_accepts_host_effects(),
                "idle Ctrl+C must not drop AutoAccept into Normal; mode={mode:?}"
            );
            let mut found_shutdown = false;
            while let Ok(event) = event_loop.event_rx.try_recv() {
                if matches!(event, super::ReplEvent::Shutdown) {
                    found_shutdown = true;
                }
            }
            assert!(
                found_shutdown,
                "idle Ctrl+C in AutoAccept must exit Finch by sending Shutdown"
            );
        })
        .await;
}

/// AutoAccept does not inherit Planning's write restriction. The executor
/// still runs write; the skipped dialog is the only change.
#[tokio::test]
async fn test_auto_accept_executor_allows_write() {
    use crate::cli::repl::ReplMode;
    use crate::tools::{PermissionManager, ToolExecutor, ToolRegistry, ToolUse};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(PlanningWriteProbe));
    let tempdir = tempfile::tempdir().expect("isolated tool-pattern store for auto-accept write");
    let executor = ToolExecutor::new(
        registry,
        PermissionManager::new(),
        tempdir.path().join("patterns.json"),
    )
    .expect("construct executor for auto-accept write");
    let auto_accept = Arc::new(RwLock::new(ReplMode::AutoAccept));
    let tool_use = ToolUse::new(
        "write".to_string(),
        serde_json::json!({"path": "src/lib.rs", "content": "allowed in auto-accept"}),
    );
    let result = executor
        .execute_tool(
            &tool_use,
            None::<fn() -> anyhow::Result<()>>,
            Some(auto_accept), // repl_mode
            None,              // plan_content
            None,              // live_output
            None,              // effect_audit
        )
        .await
        .expect("auto-accept write returns ToolResult");
    assert!(
        !result.is_error,
        "AutoAccept must not apply the Planning write restriction; content={:?}",
        result.content
    );
    assert_eq!(
        result.content, "must-not-execute-in-planning",
        "the write probe body must run under AutoAccept; content={:?}",
        result.content
    );
}

#[test]
fn test_auto_accept_remote_tool_decision_is_approve_once() {
    let tool_use = crate::tools::ToolUse::new(
        "write".to_string(),
        serde_json::json!({"path": "src/lib.rs"}),
    );
    let decision =
        super::auto_accept_remote_decision(&super::RemoteBrainApprovalKind::Tool(tool_use.clone()));
    assert_eq!(
        decision,
        serde_json::json!({"choice": "approve_once"}),
        "remote AutoAccept must send a choice confirmation_from_audit_value accepts; decision={decision}"
    );
    let confirmation = super::confirmation_from_audit_value(&decision, &tool_use)
        .expect("approve_once must decode; approve_session becomes Deny");
    assert!(
        matches!(
            confirmation,
            crate::cli::repl_event::events::ConfirmationResult::ApproveOnce
        ),
        "decoded remote auto-accept must be ApproveOnce, not Deny; confirmation={confirmation:?}"
    );
}

#[test]
fn test_cycle_mode_is_not_plan_toggle() {
    use crate::cli::commands::Command;
    assert!(
        matches!(Command::parse("/plan"), Some(Command::PlanModeToggle)),
        "/plan with no args must stay PlanModeToggle so it still enters Planning"
    );
    assert!(
        matches!(Command::parse("/cycle-mode"), Some(Command::CycleMode)),
        "Shift+Tab submits /cycle-mode, not /plan"
    );
}

fn named_brain_turn_fixture(
    response_tx: tokio::sync::oneshot::Sender<
        std::result::Result<crate::server::RunnerTurnResult, crate::server::RunnerTurnError>,
    >,
) -> super::PendingNamedBrainTurn {
    super::PendingNamedBrainTurn {
        brain: "home".into(),
        run_id: crate::brain::RunId(uuid::Uuid::nil()),
        response_tx,
        turn_events: Vec::new(),
        effect_journal: Vec::new(),
        cancellation_requested: false,
        active_tool_ids: Default::default(),
        approval_audience: crate::brain::BrainApprovalAudience {
            brain_id: crate::brain::BrainId(uuid::Uuid::nil()),
            brain: "home".into(),
            attachment_id: crate::brain::AttachmentId(uuid::Uuid::nil()),
            subject: "driver@box.local".into(),
            role: crate::brain::AttachmentRole::Driver,
            environment_generation: 1,
        },
        approval_tx: None,
        effect_audit: None,
        restart: None,
    }
}

/// AutoAccept skips the local tool dialog at the presenter, after named-Brain
/// audit events are recorded.
#[tokio::test]
async fn test_auto_accept_tool_presenter_approves_once_without_dialog() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::AutoAccept;
            let query_id = uuid::Uuid::new_v4();
            let (turn_tx, _turn_rx) = tokio::sync::oneshot::channel();
            event_loop
                .pending_named_brain_turns
                .insert(query_id, named_brain_turn_fixture(turn_tx));
            let tool_use = crate::tools::ToolUse::new(
                "write".to_string(),
                serde_json::json!({"path": "src/lib.rs", "content": "x"}),
            );
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            event_loop
                .handle_tool_approval_request(query_id, tool_use, Vec::new(), response_tx)
                .await
                .expect("auto-accept tool presenter must succeed");
            let confirmation = response_rx
                .await
                .expect("auto-accept must send ApproveOnce without a dialog");
            assert!(
                matches!(
                    confirmation,
                    crate::cli::repl_event::events::ConfirmationResult::ApproveOnce
                ),
                "tool presenter must approve once; confirmation={confirmation:?}"
            );
            let tui = event_loop.tui_renderer.lock().await;
            assert!(
                tui.active_dialog.is_none(),
                "AutoAccept must not open the tool dialog"
            );
            drop(tui);
            let turn = event_loop
                .pending_named_brain_turns
                .get(&query_id)
                .expect("named-Brain turn must remain");
            let kinds: Vec<&str> = turn
                .turn_events
                .iter()
                .map(|event| match event {
                    crate::server::RunnerTurnEvent::ApprovalRequested { .. } => "requested",
                    crate::server::RunnerTurnEvent::ApprovalDecided { decision, .. } => {
                        assert_eq!(
                            decision.get("choice").and_then(serde_json::Value::as_str),
                            Some("approve_once"),
                            "named-Brain auto-accept must record approve_once; decision={decision}"
                        );
                        "decided"
                    }
                    _ => "other",
                })
                .collect();
            assert_eq!(
                kinds,
                ["requested", "decided"],
                "named-Brain AutoAccept must audit request then decision; events={kinds:?}"
            );
        })
        .await;
}

/// Regression for #899: three tool-approval requests land in
/// `pending_approvals` for three different query_ids (a subagent's own tool
/// approval racing the parent turn's is the real-world shape). Only the
/// third dialog shown is what the user can actually see and answer; the
/// answer must resolve exactly that query_id, not an arbitrary HashMap
/// entry, and the other two must remain pending rather than being silently
/// dropped or wrongly resolved.
#[tokio::test]
async fn test_dialog_result_resolves_the_actually_shown_tool_approval_not_an_arbitrary_one() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            // Default mode is Normal -- real dialogs, no AutoAccept short-circuit.

            let mut receivers = Vec::new();
            for name in ["read", "glob", "grep"] {
                let query_id = uuid::Uuid::new_v4();
                let tool_use = crate::tools::ToolUse::new(name.to_string(), serde_json::json!({}));
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                event_loop
                    .handle_tool_approval_request(query_id, tool_use, Vec::new(), response_tx)
                    .await
                    .expect("tool approval request must be accepted");
                receivers.push((query_id, response_rx));
            }

            assert_eq!(
                event_loop.pending_approvals.read().await.len(),
                3,
                "all three approval requests must remain pending until answered"
            );
            let (last_query_id, _) = receivers[2];
            assert_eq!(
                event_loop.active_tool_approval,
                Some(last_query_id),
                "the most recently shown dialog's query_id must be tracked as active"
            );

            // The user answers the dialog they can actually see (index 0 == "Yes").
            event_loop
                .resolve_dialog_result(crate::cli::tui::DialogResult::Selected(0))
                .await
                .expect("resolving the dialog result must succeed");

            for (i, (query_id, mut response_rx)) in receivers.into_iter().enumerate() {
                if i == 2 {
                    let confirmation = response_rx.await.expect(
                        "the query_id whose dialog was actually shown must receive the answer",
                    );
                    assert!(
                        matches!(
                            confirmation,
                            crate::cli::repl_event::events::ConfirmationResult::ApproveOnce
                        ),
                        "expected ApproveOnce for the answered dialog; got {confirmation:?}"
                    );
                    assert!(
                        !event_loop
                            .pending_approvals
                            .read()
                            .await
                            .contains_key(&query_id),
                        "the answered approval must be removed from pending_approvals"
                    );
                } else {
                    assert!(
                        response_rx.try_recv().is_err(),
                        "query {query_id} was never shown its dialog and must not have been \
                         resolved by an answer meant for a different approval (#899)"
                    );
                    assert!(
                        event_loop
                            .pending_approvals
                            .read()
                            .await
                            .contains_key(&query_id),
                        "an unanswered approval must remain pending, not be silently dropped"
                    );
                }
            }
        })
        .await;
}

#[tokio::test]
async fn test_auto_accept_does_not_waive_permission_deny() {
    use crate::cli::repl::ReplMode;
    use crate::tools::{PermissionManager, PermissionRule, ToolExecutor, ToolRegistry, ToolUse};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(PlanningWriteProbe));
    let tempdir = tempfile::tempdir().expect("isolated deny store");
    let executor = ToolExecutor::new(
        registry,
        PermissionManager::new().with_default_rule(PermissionRule::Deny),
        tempdir.path().join("patterns.json"),
    )
    .expect("construct executor with deny default");
    let auto_accept = Arc::new(RwLock::new(ReplMode::AutoAccept));
    let tool_use = ToolUse::new(
        "write".to_string(),
        serde_json::json!({"path": "src/lib.rs", "content": "denied"}),
    );
    let result = executor
        .execute_tool(
            &tool_use,
            None::<fn() -> anyhow::Result<()>>,
            Some(auto_accept), // repl_mode
            None,              // plan_content
            None,              // live_output
            None,              // effect_audit
        )
        .await
        .expect("deny returns ToolResult");
    assert!(
        result.is_error,
        "AutoAccept must not waive PermissionManager Deny; content={:?}",
        result.content
    );
    assert!(
        result.content.contains("not allowed"),
        "deny must name the refusal; content={:?}",
        result.content
    );
}

#[tokio::test]
async fn test_shift_tab_enters_auto_accept_and_plan_stays_planning() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            event_loop
                .handle_user_input("/cycle-mode".to_string())
                .await
                .expect("cycle-mode must dispatch");
            assert!(
                event_loop.mode.read().await.auto_accepts_host_effects(),
                "Shift+Tab from Normal must enter AutoAccept"
            );
            event_loop
                .handle_user_input("/plan".to_string())
                .await
                .expect("/plan must dispatch");
            assert!(
                matches!(
                    *event_loop.mode.read().await,
                    crate::cli::repl::ReplMode::Planning { .. }
                ),
                "/plan from AutoAccept must enter Planning, not stay in AutoAccept; mode={:?}",
                event_loop.mode.read().await.clone()
            );
        })
        .await;
}

fn runner_recovery_test_event_loop() -> super::EventLoop {
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    let tempdir = tempfile::tempdir().expect("runner recovery fixture: isolated tool state");
    let executor = crate::tools::ToolExecutor::new(
        crate::tools::ToolRegistry::new(),
        crate::tools::PermissionManager::new(),
        tempdir.path().join("patterns.json"),
    )
    .expect("runner recovery fixture: construct inert tool executor");
    let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
    let event_loop = super::EventLoop::new_named_brain_test_runner(
        generator,
        Vec::new(),
        Arc::new(tokio::sync::Mutex::new(executor)),
        runtime,
    );
    event_loop.output_manager.disable_stdout();
    std::mem::forget(tempdir);
    event_loop
}

fn runner_recovery_messages(event_loop: &super::EventLoop) -> Vec<String> {
    event_loop
        .output_manager
        .get_messages()
        .iter()
        .map(|message| message.content())
        .collect()
}

#[tokio::test]
async fn register_home_brain_outer_err_reaches_tui_and_does_not_attach() {
    tokio::task::LocalSet::new().run_until(async {
    let mut event_loop = runner_recovery_test_event_loop();
    let error =
        crate::ipc::leftover_daemon_message(crate::ipc::IPC_PROTOCOL_VERSION, 8, Some(7_200));
    let attach = event_loop.apply_home_runner_startup(Err(anyhow::anyhow!(error)));
    assert!(
        !attach,
        "a leftover daemon must not attach as a mute driver after register_home_brain fails"
    );
    let header = event_loop
        .status_bar
        .get_line(&crate::cli::status_bar::StatusLineType::SessionLabel)
        .expect("startup must project a session header");
    assert!(
        header.contains("finch daemon-stop") || header.contains("leftover daemon"),
        "header must name leftover-daemon recovery, not daemon offline; header={header}"
    );
    assert!(
        !header.contains("daemon offline"),
        "outer register Err must not collapse into daemon-offline; header={header}"
    );
    let messages = runner_recovery_messages(&event_loop);
    assert!(
        messages.iter().any(|message| {
            message.contains("speaks 8")
                && message.contains(&format!("protocol {}", crate::ipc::IPC_PROTOCOL_VERSION))
                && message.contains("finch daemon-stop")
        }),
        "the leftover error must reach the TUI with both generations and the kick command; messages={messages:?}"
    );
    }).await;
}

#[tokio::test]
async fn workspace_mismatch_outer_err_is_visible_and_not_protocol_mismatch() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut event_loop = runner_recovery_test_event_loop();
            let attach = event_loop.apply_home_runner_startup(Err(anyhow::anyhow!(
        "frontend workspace does not match the Brain environment (expected /tmp/a, found /tmp/b)"
    )));
            assert!(
                !attach,
                "workspace mismatch must not attach as a mute driver"
            );
            let messages = runner_recovery_messages(&event_loop);
            assert!(
                messages
                    .iter()
                    .any(|message| message.contains("/tmp/a") && message.contains("/tmp/b")),
                "workspace mismatch must name expected vs found; messages={messages:?}"
            );
            assert!(
                messages
                    .iter()
                    .all(|message| !message.contains("speaks protocol")
                        && !message.contains("leftover daemon")),
                "workspace mismatch is not a leftover-daemon protocol error; messages={messages:?}"
            );
            let header = event_loop
                .status_bar
                .get_line(&crate::cli::status_bar::StatusLineType::SessionLabel)
                .expect("startup must project a session header");
            assert!(
                header.contains("workspace mismatch"),
                "header must distinguish workspace mismatch from leftover daemon; header={header}"
            );
        })
        .await;
}

#[test]
fn queued_run_projection_uses_human_labels_not_debug_enum() {
    use crate::brain::{BrainRunKind, BrainRunStatus, RunId};
    let output =
        crate::cli::output_manager::OutputManager::new(crate::theme::ColorScheme::default());
    output.disable_stdout();
    let run_id = RunId(uuid::Uuid::new_v4());
    let mut projections = std::collections::HashMap::new();
    let recovery = crate::cli::repl_event::runner_recovery::RunnerRecovery::ProtocolMismatch {
        frontend: crate::ipc::IPC_PROTOCOL_VERSION,
        daemon: 8,
        uptime_seconds: Some(120),
    };
    super::ensure_remote_brain_run_projection(
        &output,
        &mut projections,
        run_id,
        Some(BrainRunKind::Interactive),
        BrainRunStatus::QueuedForEnvironment,
        Some(&recovery),
    );
    let unit = projections.get(&run_id).unwrap().unit.clone();
    let projected = crate::cli::test_projection::try_project_for_test(
        unit.as_ref(),
        &crate::theme::ColorScheme::default(),
    )
    .unwrap();
    let haystack = format!("{} {:?}", projected.label, projected.children);
    assert!(
        !haystack.to_lowercase().contains("queuedforenvironment"),
        "queued-run rows must use human labels; haystack={haystack}"
    );
    assert!(
        haystack.contains("leftover daemon") && haystack.contains("finch daemon-stop"),
        "queued leftover rows must name the kick command; haystack={haystack}"
    );
}

#[tokio::test]
async fn other_owner_inner_failure_still_allows_driver_attach() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut event_loop = runner_recovery_test_event_loop();
            let attach =
                event_loop.apply_home_runner_startup(Ok(Some(super::HomeRunnerRegistration {
                    target: crate::cli::repl_event::events::RunnerReconnectTarget {
                        brain: "slow-grove-155efd".into(),
                        environment: crate::brain::BrainEnvironment {
                            machine: "box.local".into(),
                            workspace: std::path::PathBuf::from("/tmp/ws"),
                            generation: 1,
                        },
                        lease_id: None,
                    },
                    registration: Err(
                        "Brain runner lease belongs to another subject (alice@host/frontend)"
                            .into(),
                    ),
                })));
            assert!(
        attach,
        "another owner is observable as a driver so handoff can be requested; attach={attach}"
    );
            let messages = runner_recovery_messages(&event_loop);
            assert!(
                messages
                    .iter()
                    .any(|message| message.contains("alice@host/frontend")
                        && message.contains("handoff")),
                "other-owner recovery must name the owner and handoff; messages={messages:?}"
            );
        })
        .await;
}

fn transcript_projection_haystack(event_loop: &super::EventLoop) -> String {
    let colors = crate::theme::ColorScheme::default();
    event_loop
        .output_manager
        .get_messages()
        .iter()
        .map(|message| {
            format!(
                "{}\n{}",
                message.content(),
                message.complete_transcript(&colors)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn peer_disconnect_environment() -> crate::brain::BrainEnvironment {
    crate::brain::BrainEnvironment {
        machine: "box.local".into(),
        workspace: std::path::PathBuf::from("/tmp/ws"),
        generation: 1,
    }
}

#[tokio::test]
async fn peer_ipc_diagnostics_after_completed_turn_stay_off_transcript() {
    tokio::task::LocalSet::new()
        .run_until(peer_ipc_diagnostics_after_completed_turn_stay_off_transcript_scenario())
        .await;
}

async fn peer_ipc_diagnostics_after_completed_turn_stay_off_transcript_scenario() {
    let mut event_loop = runner_recovery_test_event_loop();
    event_loop
        .handle_event(super::ReplEvent::LispResult {
            result: Ok("Hello, Shammah!".into()),
        })
        .await
        .expect("completed say/Lisp turn must dispatch");

    event_loop
        .handle_event(super::ReplEvent::RemoteBrainError {
            target: "quiet-peak-715803".into(),
            error: "Disconnected: Peer disconnected.".into(),
        })
        .await
        .expect("peer disconnect must dispatch");
    event_loop
        .handle_event(super::ReplEvent::HomeBrainWatchFailed {
            epoch: 0,
            error: Some("Disconnected: Peer disconnected.".into()),
        })
        .await
        .expect("home event-watch failure must dispatch");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        event_loop.handle_event(super::ReplEvent::ReconnectHomeBrain {
            epoch: 0,
            attempt: 0,
        }),
    )
    .await
    .expect("home reconnect must not hang on isolated IPC")
    .expect("home reconnect dispatch must settle");
    event_loop
        .handle_event(super::ReplEvent::RunnerLeaseStatus {
            brain: "quiet-peak-715803".into(),
            environment: peer_disconnect_environment(),
            epoch: 0,
            lease_id: None,
            detail: "Disconnected: Peer disconnected.".into(),
        })
        .await
        .expect("runner lease loss must dispatch");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        event_loop.handle_event(super::ReplEvent::ReconnectHomeRunner {
            epoch: 0,
            attempt: 0,
            target: crate::cli::repl_event::events::RunnerReconnectTarget {
                brain: "quiet-peak-715803".into(),
                environment: peer_disconnect_environment(),
                lease_id: None,
            },
        }),
    )
    .await
    .expect("runner reconnect must not hang on isolated IPC")
    .expect("runner reconnect dispatch must settle");

    let haystack = transcript_projection_haystack(&event_loop);
    assert!(
        haystack.contains("Hello, Shammah!"),
        "completed turn must remain in the transcript; haystack={haystack:?}"
    );
    for needle in [
        "Peer disconnected",
        "event watch unavailable",
        "reconnect attempt failed",
    ] {
        assert!(
            !haystack.contains(needle),
            "peer/home/runner IPC diagnostics must not become sticky transcript rows after a completed turn; needle={needle:?} haystack={haystack:?}"
        );
    }

    let header = event_loop
        .status_bar
        .get_line(&crate::cli::status_bar::StatusLineType::SessionLabel)
        .unwrap_or_default();
    let status = event_loop.status_bar.get_status();
    let recovery = format!("{header}\n{status}");
    assert!(
        recovery.contains("no runner lease")
            || recovery.contains("event watch reconnecting")
            || recovery.contains("disconnected")
            || recovery.contains("daemon IPC")
            || recovery.contains("runner unavailable"),
        "header/status must still show a compact runner/home recovery hint; header={header:?} status={status:?} haystack={haystack:?}"
    );
}

#[test]
fn test_resume_instruction_uses_validated_brain_name() {
    assert_eq!(
        super::interactive_resume_instruction("golden-ridge-0771a6", true),
        "To resume, run: finch attach golden-ridge-0771a6"
    );
}

#[test]
fn test_resume_instruction_fails_closed_without_durable_brain() {
    let line = super::interactive_resume_instruction("golden-ridge-0771a6", false);
    assert!(
        line.contains("cannot be resumed"),
        "unavailable persistence must not claim resume, got {line}"
    );
    assert!(
        !line.contains("finch attach"),
        "unavailable persistence must not print an attach command, got {line}"
    );
}

#[test]
fn test_resume_instruction_rejects_hostile_brain_names() {
    for name in ["evil; rm -rf /", "x`id`", "\u{1b}[31mred", "two words"] {
        let line = super::interactive_resume_instruction(name, true);
        assert!(
            !line.contains("finch attach"),
            "hostile name {name:?} must not become a copyable command, got {line}"
        );
        assert!(
            !line.contains(';') && !line.contains('`') && !line.contains('\u{1b}'),
            "hostile name {name:?} must not leak shell or control characters, got {line}"
        );
    }
}
