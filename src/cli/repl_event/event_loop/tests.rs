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

/// #910 at the turn dispatch boundary: a turn that failed with tool rows
/// still running resolves those rows with the query's failure, while rows
/// that already completed keep their own outcomes.
#[tokio::test]
async fn test_turn_failure_resolves_stuck_tool_rows_with_query_error() {
    tokio::task::LocalSet::new()
        .run_until(turn_failure_resolves_stuck_tool_rows_scenario())
        .await;
}

async fn turn_failure_resolves_stuck_tool_rows_scenario() {
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
    let query_id = event_loop.query_states.create_query(Vec::new()).await;
    let unit = Arc::new(crate::cli::messages::WorkUnit::new("Tools"));
    let done_row = unit.add_row("bash(git status)");
    unit.complete_row(done_row, "clean");
    let stuck_row = unit.add_row("bash(sleep 30)");
    event_loop
        .query_states
        .set_tool_work_unit(query_id, Some(Arc::clone(&unit)))
        .await;

    let error = "provider stream failed";
    event_loop
        .handle_event(super::ReplEvent::QueryFailed {
            query_id,
            error: error.to_string(),
            generator_name: None,
        })
        .await
        .expect("turn-failure dispatch must succeed");

    let report = run_group_row_report(&unit);
    let view = unit.domain_view(&crate::theme::ColorScheme::default());
    assert_eq!(
        view.rows[stuck_row].status,
        crate::cli::messages::WorkRowStatus::Error(error.to_string()),
        "INVARIANT: a tool row still running when its turn failed must resolve with the \
         query's failure instead of running forever; row index {stuck_row}\nrows:\n  {report}"
    );
    assert_eq!(
        view.rows[done_row].status,
        crate::cli::messages::WorkRowStatus::Complete("clean".to_string()),
        "the row that already completed keeps its own outcome; row index {done_row}\nrows:\n  {report}"
    );
    assert_eq!(
        view.head.status,
        crate::cli::messages::MessageStatus::Failed,
        "the turn unit head must be failed; rows:\n  {report}"
    );
}

/// A `QueryFailed` event names the generator/provider it actually ran on
/// (when known), so the failure message is self-explanatory even when an
/// unrelated model-switch notification lands nearby in the transcript.
/// `None` falls back to the pre-attribution message unchanged.
#[tokio::test]
async fn test_query_failed_message_attributes_the_generator_when_known() {
    tokio::task::LocalSet::new()
        .run_until(query_failed_message_attribution_scenario())
        .await;
}

async fn query_failed_message_attribution_scenario() {
    use std::sync::Arc;

    async fn dispatch_query_failed(
        generator_name: Option<String>,
    ) -> Vec<crate::cli::messages::MessageRef> {
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
        let query_id = event_loop.query_states.create_query(Vec::new()).await;

        event_loop
            .handle_event(super::ReplEvent::QueryFailed {
                query_id,
                error: "429 rate limit".to_string(),
                generator_name,
            })
            .await
            .expect("QueryFailed dispatch must succeed");

        event_loop.output_manager.get_messages()
    }

    let attributed = dispatch_query_failed(Some("chatgpt".to_string())).await;
    let attributed_text = attributed
        .iter()
        .map(|message| message.format(&crate::theme::ColorScheme::default()))
        .collect::<Vec<_>>();
    assert!(
        attributed_text
            .iter()
            .any(|text| text.contains("Query failed (chatgpt): 429 rate limit")),
        "a QueryFailed event with a known generator must name it in the failure message; \
         rendered={attributed_text:?}"
    );

    let unattributed = dispatch_query_failed(None).await;
    let unattributed_text = unattributed
        .iter()
        .map(|message| message.format(&crate::theme::ColorScheme::default()))
        .collect::<Vec<_>>();
    assert!(
        unattributed_text
            .iter()
            .any(|text| text.contains("Query failed: 429 rate limit")),
        "a QueryFailed event with no known generator must fall back to the unattributed \
         message unchanged; rendered={unattributed_text:?}"
    );
    assert!(
        unattributed_text
            .iter()
            .all(|text| !text.contains("Query failed (")),
        "the unattributed fallback must not contain a stray provider parenthetical; \
         rendered={unattributed_text:?}"
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
fn test_canonical_brain_context_attributes_interactive_result_to_assistant() {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };

    let run_id = RunId(uuid::Uuid::new_v4());
    let mut started = brain_event(
        1,
        "daemon",
        BrainEventKind::RunStarted {
            run: BrainRun {
                run_id,
                kind: BrainRunKind::Interactive,
                parent_run_id: None,
                request_seq: 0,
                initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
                initiated_by: "alice".into(),
                status: BrainRunStatus::Running,
                started_ms: 1,
                updated_ms: 1,
                detail: None,
            },
        },
    );
    started.run_id = Some(run_id);
    let mut result = brain_event(
        2,
        "daemon",
        BrainEventKind::Result {
            request_seq: 0,
            output: "semantic answer".into(),
            error: None,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
    );
    result.run_id = Some(run_id);

    assert_eq!(
        projected_brain_context_lines(&[started, result], 2, None),
        vec!["assistant: semantic answer"],
        "INVARIANT: an Interactive Result is assistant conversation, never daemon speech"
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

/// Issue #422: an attached console must project a completed Interactive run
/// as the semantic turn it represents, not as an opaque lifecycle group
/// named by its internal RunId. The prompt is already its own canonical
/// participant row; this correlated unit owns the inspectable program, tool
/// exchange, assistant result, and terminal outcome exactly once.
#[test]
fn test_interactive_run_projects_semantic_turn_without_uuid_lifecycle_chrome() {
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
        kind: BrainRunKind::Interactive,
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
        BrainEventKind::Program {
            language: ProgramLanguage::Lisp,
            source: "(read \"notes.txt\")\n(say \"done\")".into(),
        },
        BrainEventKind::ToolCall {
            request_seq: 1,
            tool_id: "call-read".into(),
            name: "read".into(),
            input: serde_json::json!({"file_path": "notes.txt"}),
        },
        BrainEventKind::ApprovalRequested {
            request_seq: 1,
            approval_id: "call-read".into(),
            approval_kind: "tool".into(),
            subject: "read".into(),
            audience: None,
            detail: serde_json::json!({"input": {"file_path": "notes.txt"}}),
        },
        BrainEventKind::ApprovalDecided {
            request_seq: 1,
            approval_id: "call-read".into(),
            decision: serde_json::json!({"choice": "approve_once"}),
        },
        BrainEventKind::ToolResult {
            request_seq: 1,
            tool_id: "call-read".into(),
            output: "contents".into(),
            is_error: false,
        },
        BrainEventKind::Result {
            request_seq: 1,
            output: "done".into(),
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
    let mut projections = std::collections::HashMap::new();
    for (index, kind) in kinds.into_iter().enumerate() {
        let mut event = brain_event(index as u64 + 2, "daemon", kind);
        event.run_id = Some(run_id);
        assert!(
            super::project_remote_brain_run_event(
                &output,
                &mut projections,
                &event,
                &super::LocallyRenderedRuns::default(),
                None,
            ),
            "INVARIANT: every correlated Interactive event is consumed by the semantic run projection; event={event:?}"
        );
    }

    let unit = projections
        .get(&run_id)
        .expect("interactive run unit")
        .unit
        .clone();
    let projected = crate::cli::test_projection::try_project_for_test(
        unit.as_ref(),
        &crate::theme::ColorScheme::default(),
    )
    .expect("interactive run projection");
    let rendered = format!("{projected:#?}");
    let canonical = crate::cli::messages::Message::complete_transcript(
        unit.as_ref(),
        &crate::theme::ColorScheme::default(),
    );
    let run_uuid = run_id.0.to_string();

    assert!(
        !rendered.contains(&run_uuid) && !canonical.contains(&run_uuid),
        "INVARIANT: an Interactive transcript exposes semantic turn content, never opaque RunId lifecycle chrome; projection={rendered}\ncanonical=\n{canonical}"
    );
    assert_eq!(
        canonical.matches("(read \"notes.txt\")").count(),
        1,
        "INVARIANT: canonical scrollback retains the inspectable program exactly once; canonical=\n{canonical}"
    );
    assert_eq!(
        projected.body,
        vec!["done"],
        "INVARIANT: the semantic root carries the assistant result once; projection={rendered}"
    );
    assert_eq!(
        canonical.matches("⏺ done").count(),
        1,
        "INVARIANT: the assistant result is canonical conversation output exactly once; canonical=\n{canonical}"
    );
    assert!(
        !canonical.lines().any(|line| line.contains("result —")),
        "INVARIANT: assistant output is not hidden behind an internal result lifecycle row; canonical=\n{canonical}"
    );
    assert!(
        canonical.contains("approve_once by daemon") && canonical.contains("contents"),
        "INVARIANT: the gated tool row retains its approval and result details; canonical=\n{canonical}"
    );
    assert!(
        crate::cli::messages::Message::status(unit.as_ref())
            == crate::cli::messages::MessageStatus::Complete
            && !canonical.contains("status — running"),
        "INVARIANT: the terminal outcome is complete and no stale running state survives; status={:?}; canonical=\n{canonical}",
        crate::cli::messages::Message::status(unit.as_ref())
    );
}

#[test]
fn test_interactive_run_failure_and_cancel_project_one_actionable_terminal_outcome() {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };
    use crate::cli::messages::{Message, MessageStatus};

    for (terminal, detail) in [
        (BrainRunStatus::Failed, "provider authentication failed"),
        (BrainRunStatus::Cancelled, "cancelled by alice"),
    ] {
        let output = replay_output_manager();
        let run_id = RunId(uuid::Uuid::new_v4());
        let run = BrainRun {
            run_id,
            kind: BrainRunKind::Interactive,
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
            BrainEventKind::RunStatusChanged {
                run_id,
                status: terminal,
                detail: Some(detail.into()),
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

        let unit = &projections.get(&run_id).expect("run projection").unit;
        let canonical = unit.complete_transcript(&crate::theme::ColorScheme::default());
        assert_eq!(
            unit.status(),
            MessageStatus::Failed,
            "INVARIANT: {terminal:?} is a terminal failed presentation; canonical=\n{canonical}"
        );
        assert_eq!(
            canonical.matches(detail).count(),
            1,
            "INVARIANT: {terminal:?} renders one actionable outcome; canonical=\n{canonical}"
        );
        assert!(
            !canonical.contains(&run_id.0.to_string())
                && !canonical.contains("status — running")
                && !canonical.contains("daemon"),
            "INVARIANT: {terminal:?} contains no UUID lifecycle chrome, stale state, or daemon attribution; canonical=\n{canonical}"
        );
    }
}

/// Row report for #910 failure payloads: label and resolved status per row.
fn run_group_row_report(unit: &crate::cli::messages::WorkUnit) -> String {
    let view = unit.domain_view(&crate::theme::ColorScheme::default());
    view.rows
        .iter()
        .map(|row| format!("{:?} :: {}", row.status, row.label))
        .collect::<Vec<_>>()
        .join("\n  ")
}

/// #910 regression at the run-group projection boundary — the issue's real
/// transcript capture. A peer disconnect mid-run resolves the parent run as
/// failed, and its still-in-flight child tool and approval rows must resolve
/// with that failure instead of freezing at `running`.
#[test]
fn test_run_terminal_status_resolves_stuck_child_rows_on_disconnect() {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };
    use crate::cli::messages::WorkRowStatus;

    let output = replay_output_manager();
    let run_id = RunId(uuid::Uuid::new_v4());
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Interactive,
        parent_run_id: None,
        request_seq: 1,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "peer".into(),
        status: BrainRunStatus::Running,
        started_ms: 1,
        updated_ms: 1,
        detail: None,
    };
    let disconnect_detail = "Disconnected: Peer disconnected.";
    let kinds = [
        BrainEventKind::RunStarted { run },
        BrainEventKind::ToolCall {
            request_seq: 1,
            tool_id: "tool-1".into(),
            name: "enter_plan_mode".into(),
            input: serde_json::json!({"task": "identify a small open bug ticket"}),
        },
        BrainEventKind::ApprovalRequested {
            request_seq: 1,
            approval_id: "approval-1".into(),
            approval_kind: "tool".into(),
            subject: "enter_plan_mode".into(),
            audience: None,
            detail: serde_json::Value::Null,
        },
        BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Failed,
            detail: Some(disconnect_detail.to_string()),
        },
        BrainEventKind::Result {
            request_seq: 1,
            output: String::new(),
            error: Some(disconnect_detail.to_string()),
            continuation_messages: Vec::new(),
            invocation_metadata: None,
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
            "the disconnect capture's event {index} must be handled by the run projection"
        );
    }

    let unit = projections.get(&run_id).unwrap().unit.clone();
    let view = unit.domain_view(&crate::theme::ColorScheme::default());
    let stuck: Vec<String> = view
        .rows
        .iter()
        .filter(|row| matches!(row.status, WorkRowStatus::Running))
        .map(|row| row.label.clone())
        .collect();
    assert!(
        stuck.is_empty(),
        "INVARIANT: no child row stays running after the parent run resolved failed \
         (Disconnected: Peer disconnected.); stuck rows={stuck:?}\nrows:\n  {}",
        run_group_row_report(&unit)
    );
    let tool_row = view
        .rows
        .iter()
        .find(|row| row.label.contains("enter_plan_mode") && !row.label.contains("approval"))
        .unwrap_or_else(|| {
            panic!(
                "tool row must exist; rows:\n  {}",
                run_group_row_report(&unit)
            )
        });
    assert_eq!(
        tool_row.status,
        WorkRowStatus::Error(disconnect_detail.to_string()),
        "the still-running enter_plan_mode tool row must resolve with the run's disconnect \
         failure; rows:\n  {}",
        run_group_row_report(&unit)
    );
    let approval_row = view
        .rows
        .iter()
        .find(|row| row.label.contains("approval (tool)"))
        .unwrap_or_else(|| {
            panic!(
                "approval row must exist; rows:\n  {}",
                run_group_row_report(&unit)
            )
        });
    assert_eq!(
        approval_row.status,
        WorkRowStatus::Error(disconnect_detail.to_string()),
        "the still-running approval row must resolve with the run's disconnect failure; \
         rows:\n  {}",
        run_group_row_report(&unit)
    );

    let projected = crate::cli::test_projection::try_project_for_test(
        unit.as_ref(),
        &crate::theme::ColorScheme::default(),
    )
    .unwrap();
    assert_eq!(
        projected.role,
        crate::cli::test_projection::NodeRole::Response,
        "the failed Interactive run remains a semantic turn; projection={projected:?}"
    );
    assert_eq!(
        projected.body,
        vec![disconnect_detail],
        "the semantic turn must expose the actionable failure once; projection={projected:?}"
    );
    assert!(
        !projected
            .children
            .iter()
            .any(|child| child.label.contains("— running")),
        "no projected child row may still read running; children={:?}",
        projected
            .children
            .iter()
            .map(|child| child.label.clone())
            .collect::<Vec<_>>()
    );
}

/// #910: a run that completes while a child tool row never received its own
/// result resolves that row with the run, and child rows that already carry
/// their own terminal outcome before the terminal status keep it.
#[test]
fn test_run_terminal_status_resolves_stuck_child_rows_on_success() {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };
    use crate::cli::messages::{Message, MessageStatus, WorkRowStatus};

    let output = replay_output_manager();
    let run_id = RunId(uuid::Uuid::new_v4());
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Interactive,
        parent_run_id: None,
        request_seq: 1,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "peer".into(),
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
        BrainEventKind::Result {
            request_seq: 1,
            output: "done".into(),
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
    let view = unit.domain_view(&crate::theme::ColorScheme::default());
    let tool_row = view
        .rows
        .iter()
        .find(|row| row.label.contains("read_cache"))
        .unwrap_or_else(|| {
            panic!(
                "tool row must exist; rows:\n  {}",
                run_group_row_report(&unit)
            )
        });
    assert_eq!(
        tool_row.status,
        WorkRowStatus::Complete("resolved by run completion".to_string()),
        "the tool row that never received its result must resolve when the run completes; \
         rows:\n  {}",
        run_group_row_report(&unit)
    );
    assert_eq!(
        unit.content(),
        "done",
        "the assistant Result belongs on the semantic root, not a lifecycle row"
    );
    assert!(
        view.rows.iter().all(|row| row.label != "result"),
        "the assistant Result must not be duplicated as a child row; rows:\n  {}",
        run_group_row_report(&unit)
    );
    assert_eq!(
        view.head.status,
        MessageStatus::Complete,
        "the run group head must be complete; rows:\n  {}",
        run_group_row_report(&unit)
    );
    assert!(
        view.rows
            .iter()
            .all(|row| !matches!(row.status, WorkRowStatus::Running)),
        "no child row stays running after the run completed; rows:\n  {}",
        run_group_row_report(&unit)
    );
}

/// #910: a failed run with no failure detail still resolves its in-flight
/// child rows with a failed status naming the run, never `running`.
#[test]
fn test_run_terminal_status_resolves_stuck_child_rows_on_failure_without_detail() {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };
    use crate::cli::messages::WorkRowStatus;

    let output = replay_output_manager();
    let run_id = RunId(uuid::Uuid::new_v4());
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Interactive,
        parent_run_id: None,
        request_seq: 1,
        initiating_attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        initiated_by: "peer".into(),
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
            name: "enter_plan_mode".into(),
            input: serde_json::json!({}),
        },
        BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Failed,
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
    let view = unit.domain_view(&crate::theme::ColorScheme::default());
    let tool_row = view
        .rows
        .iter()
        .find(|row| row.label.contains("enter_plan_mode"))
        .unwrap_or_else(|| {
            panic!(
                "tool row must exist; rows:\n  {}",
                run_group_row_report(&unit)
            )
        });
    assert_eq!(
        tool_row.status,
        WorkRowStatus::Error("run failed".to_string()),
        "a child row still running at a detail-less failed run must resolve failed; \
         rows:\n  {}",
        run_group_row_report(&unit)
    );
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

/// A minimal `LocalBrainTransport` fake whose only meaningful behavior is
/// `brain_attach`: it hands back a `BrainAttachment` with a caller-chosen,
/// nonzero `acknowledged_seq`, simulating the persisted per-`client_slot`
/// attachment cursor (`AttachmentIdentityStore`, `attach_persistent` in
/// `crates/finch-brain/src/remote.rs`) that a brand-new client process
/// inherits on reattach because `client_slot` is the stable Brain name, not a
/// per-process identity (issue #909). Every other method is unreachable: the
/// fixture below drives `render_remote_brain_message` directly and never
/// calls them.
struct StaleCursorTransport {
    acknowledged_seq: u64,
}

#[async_trait::async_trait(?Send)]
impl crate::brain::LocalBrainTransport for StaleCursorTransport {
    async fn brain_attach(
        &self,
        _brain: &str,
        subject: &str,
        role: crate::brain::AttachmentRole,
        _attachment_id: Option<crate::brain::AttachmentId>,
    ) -> anyhow::Result<crate::brain::BrainAttachment> {
        Ok(crate::brain::BrainAttachment {
            attachment_id: crate::brain::AttachmentId(uuid::Uuid::new_v4()),
            subject: subject.to_string(),
            role,
            acknowledged_seq: self.acknowledged_seq,
            connected: false,
            connection_id: None,
        })
    }
    async fn brain_snapshot(&self, _brain: &str) -> anyhow::Result<crate::brain::BrainSnapshot> {
        unreachable!("fixture drives render_remote_brain_message directly")
    }
    async fn brain_submit(
        &self,
        _brain: &str,
        _attachment: &crate::brain::BrainAttachment,
        _kind: crate::brain::BrainEventKind,
    ) -> anyhow::Result<()> {
        unreachable!()
    }
    async fn brain_start_speculative(
        &self,
        _brain: &str,
        _attachment: &crate::brain::BrainAttachment,
        _prompt: String,
    ) -> anyhow::Result<crate::brain::BrainRun> {
        unreachable!()
    }
    async fn brain_cancel_run(
        &self,
        _brain: &str,
        _attachment: &crate::brain::BrainAttachment,
        _run_id: crate::brain::RunId,
    ) -> anyhow::Result<crate::brain::BrainRun> {
        unreachable!()
    }
    async fn brain_create_schedule(
        &self,
        _brain: &str,
        _attachment: &crate::brain::BrainAttachment,
        _language: crate::brain::ProgramLanguage,
        _source: &str,
        _grant_ceiling: &finch_vm::EffectSet,
        _next_due_ms: u64,
        _interval_ms: Option<u64>,
        _delivery_policy: &crate::brain::BrainScheduleDeliveryPolicy,
    ) -> anyhow::Result<crate::brain::BrainSchedule> {
        unreachable!()
    }
    async fn brain_inspect_schedule(
        &self,
        _brain: &str,
        _schedule_id: crate::brain::ScheduleId,
    ) -> anyhow::Result<Option<crate::brain::BrainSchedule>> {
        unreachable!()
    }
    async fn brain_cancel_schedule(
        &self,
        _brain: &str,
        _attachment: &crate::brain::BrainAttachment,
        _schedule_id: crate::brain::ScheduleId,
    ) -> anyhow::Result<bool> {
        unreachable!()
    }
    async fn brain_schedule_initialization(
        &self,
        _brain: &str,
        _attachment: &crate::brain::BrainAttachment,
        _next_due_ms: u64,
    ) -> anyhow::Result<crate::brain::BrainSchedule> {
        unreachable!()
    }
    async fn brain_acknowledge(
        &self,
        _brain: &str,
        attachment: &crate::brain::BrainAttachment,
        _seq: u64,
    ) -> anyhow::Result<crate::brain::BrainAttachment> {
        Ok(attachment.clone())
    }
    async fn brain_detach(
        &self,
        _brain: &str,
        _attachment: &crate::brain::BrainAttachment,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn brain_watch(
        &self,
        _brain: &str,
        _attachment: &crate::brain::BrainAttachment,
    ) -> anyhow::Result<
        tokio::sync::mpsc::UnboundedReceiver<anyhow::Result<crate::brain::BrainWireMessage>>,
    > {
        unreachable!()
    }
}

/// Issue #909's hypothesis: a fresh client process that reattaches to an
/// existing Brain and inherits a stale, fully-caught-up `acknowledged_seq`
/// (via the persisted `client_slot` attachment cursor, simulated here by
/// `StaleCursorTransport`) might silently skip rendering prior conversation
/// turns it has never actually displayed. It does not: every interactive
/// turn is a `BrainRun` and `project_remote_brain_snapshot_runs` rebuilds run
/// groups from the full snapshot unconditionally — `acknowledged_seq` only
/// gates the separate, run-unaffiliated replay loop in
/// `render_remote_brain_message` (queue/administrative events and
/// off-run `say`s). This production-boundary test drives a real
/// `AttachedBrainClient` (backed by the fake transport, not a stub of
/// `render_remote_brain_message`'s internals) through the exact attach and
/// render path `attach_home_brain` uses, with `acknowledged_seq` set to the
/// snapshot's own revision -- the most hostile stale-cursor case, matching
/// what `attach_home_brain`'s own eager `client.acknowledge(snapshot.revision)`
/// produces before this function ever runs.
#[tokio::test]
async fn reattach_with_stale_fully_acknowledged_cursor_still_renders_every_prior_turn() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::brain::BrainRunStatus;

            let run_one = crate::brain::RunId(uuid::Uuid::new_v4());
            let run_two = crate::brain::RunId(uuid::Uuid::new_v4());
            let mut events = replayed_say_run_events(
                run_one,
                "(say \"turn one\")",
                Some("turn one"),
                None,
                Some(BrainRunStatus::Completed),
            );
            let mut second = replayed_say_run_events(
                run_two,
                "(say \"turn two\")",
                Some("turn two"),
                None,
                Some(BrainRunStatus::Completed),
            );
            events.append(&mut second);
            for (index, event) in events.iter_mut().enumerate() {
                event.seq = index as u64 + 1;
            }
            let snapshot = replayed_brain_snapshot(events);
            let stale_seq = snapshot.revision;

            let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
            let tempdir = tempfile::tempdir().expect("stale cursor fixture: isolated tool state");
            let executor = crate::tools::ToolExecutor::new(
                crate::tools::ToolRegistry::new(),
                crate::tools::PermissionManager::new(),
                tempdir.path().join("patterns.json"),
            )
            .expect("stale cursor fixture: construct inert tool executor");
            let generator: Arc<dyn crate::generators::Generator> = Arc::new(NeverCompletes);
            let mut event_loop = super::EventLoop::new_named_brain_test_runner(
                generator,
                Vec::new(),
                Arc::new(tokio::sync::Mutex::new(executor)),
                Arc::clone(&runtime),
            );
            event_loop.output_manager.disable_stdout();

            let target =
                crate::brain::RemoteBrainTarget::local("shared", "http://127.0.0.1:0").unwrap();
            let mut client = crate::brain::AttachedBrainClient::local(
                target,
                StaleCursorTransport {
                    acknowledged_seq: stale_seq,
                },
            );
            client
                .attach("shammah", crate::brain::AttachmentRole::Driver, None)
                .await
                .expect("fake attach must hand back the stale-cursor attachment");
            assert_eq!(
                client
                    .attachment()
                    .map(|attachment| attachment.acknowledged_seq),
                Some(stale_seq),
                "fixture: the attachment must actually carry the stale cursor being tested"
            );
            event_loop.home_brain = Some(client);

            event_loop
                .render_remote_brain_message(crate::brain::BrainWireMessage::Snapshot {
                    brain: snapshot,
                })
                .await
                .expect("snapshot replay must dispatch");

            let messages = event_loop.output_manager.get_messages();
            let rendered = messages
                .iter()
                .map(|message| message.format(&crate::theme::ColorScheme::default()))
                .collect::<Vec<_>>()
                .join("\n---\n");
            assert_eq!(
                messages.len(),
                2,
                "INVARIANT (#909): a fresh reattach with a stale, fully-caught-up \
                 acknowledged_seq must still render every prior interactive turn -- \
                 run-group reconstruction does not consult the acknowledgement cursor; \
                 rendered={rendered}"
            );
            assert!(
                rendered.contains("turn one") && rendered.contains("turn two"),
                "INVARIANT (#909): both historical turns' content must actually be present, \
                 not just two empty placeholders; rendered={rendered}"
            );
        })
        .await;
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

/// Issue #439 (approvals render as a separate block instead of inline with
/// the tool call they gate): a maintainer transcript capture showed every
/// tool call's approval collected into a second "Tools (N calls)" section,
/// correlated back to its gated call only by matching an opaque provider
/// call id — the denial ("failed: deny") in particular appeared detached
/// from the edit it actually blocked. `handle_tool_approval_request` always
/// sets a tool approval's `approval_id` to the gated call's own `tool_id`
/// (tools.rs), so `project_remote_brain_run_event` can and must fold the
/// decision onto that same row instead of a separately correlated one.
#[test]
fn tool_approval_decision_renders_on_the_gated_tool_row_not_a_separate_section() {
    use crate::brain::{
        AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus, RunId,
    };

    let output =
        crate::cli::output_manager::OutputManager::new(crate::theme::ColorScheme::default());
    output.disable_stdout();
    let run_id = RunId(uuid::Uuid::new_v4());
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
    // One approved edit and one denied edit, each identified the way
    // production code actually identifies them: `approval_id` equal to the
    // gated call's own `tool_id` (never a separate id space).
    let kinds = [
        BrainEventKind::RunStarted { run },
        BrainEventKind::ToolCall {
            request_seq: 1,
            tool_id: "call_approved_edit".into(),
            name: "edit".into(),
            input: serde_json::json!({"file_path": "index.html"}),
        },
        BrainEventKind::ApprovalRequested {
            request_seq: 1,
            approval_id: "call_approved_edit".into(),
            approval_kind: "tool".into(),
            subject: "edit".into(),
            audience: None,
            detail: serde_json::json!({"input": {"file_path": "index.html"}}),
        },
        BrainEventKind::ApprovalDecided {
            request_seq: 1,
            approval_id: "call_approved_edit".into(),
            decision: serde_json::json!({"choice": "approve_pattern_session"}),
        },
        BrainEventKind::ToolResult {
            request_seq: 1,
            tool_id: "call_approved_edit".into(),
            output: "Edited index.html".into(),
            is_error: false,
        },
        BrainEventKind::ToolCall {
            request_seq: 2,
            tool_id: "call_denied_edit".into(),
            name: "edit".into(),
            input: serde_json::json!({"file_path": "secrets.env"}),
        },
        BrainEventKind::ApprovalRequested {
            request_seq: 2,
            approval_id: "call_denied_edit".into(),
            approval_kind: "tool".into(),
            subject: "edit".into(),
            audience: None,
            detail: serde_json::json!({"input": {"file_path": "secrets.env"}}),
        },
        BrainEventKind::ApprovalDecided {
            request_seq: 2,
            approval_id: "call_denied_edit".into(),
            decision: serde_json::json!({"choice": "deny"}),
        },
        BrainEventKind::ToolResult {
            request_seq: 2,
            tool_id: "call_denied_edit".into(),
            output: "Tool execution denied by user".into(),
            is_error: true,
        },
        BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Completed,
            detail: None,
        },
    ];
    let mut projections = std::collections::HashMap::new();
    for (index, kind) in kinds.into_iter().enumerate() {
        let mut event = brain_event(index as u64 + 1, "shammah", kind);
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
    let dump = format!("{projected:#?}");

    // No row is reachable only by matching a provider call id: every
    // approval-only row (the pre-#439 shape) must be gone.
    assert!(
        !projected
            .children
            .iter()
            .any(|row| row.label.starts_with("approval")),
        "an approval must not render as its own row, separately correlated \
         by id to the tool call it gates: {dump}"
    );

    let tool_rows: Vec<_> = projected
        .children
        .iter()
        .filter(|row| row.role == crate::cli::test_projection::NodeRole::ToolCall)
        .collect();
    assert_eq!(
        tool_rows.len(),
        2,
        "exactly the two edit calls must project as tool rows, no extras: {dump}"
    );

    // The approved call's row carries who approved it and how, in its own
    // body -- not in a same-named row four screens away.
    let approved = tool_rows
        .iter()
        .find(|row| row.label.contains("edit") && !row.label.contains("failed"))
        .unwrap_or_else(|| panic!("the approved edit must project as a tool row: {dump}"));
    let approved_body: String = approved
        .children
        .iter()
        .flat_map(|child| child.body.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        approved_body.contains("approve_pattern_session by shammah"),
        "the approved call's own row must show who approved it and how: {dump}"
    );

    // The denial -- "the single most important fact" per #439 -- must read
    // as a denied call in place, on the row for the edit it actually
    // blocked, not on an unrelated row matched only by a call id.
    let denied = tool_rows
        .iter()
        .find(|row| row.label.contains("failed"))
        .unwrap_or_else(|| panic!("the denied edit must project as a failed tool row: {dump}"));
    assert!(
        denied.label.contains("edit"),
        "the denial must be on the edit row it blocked, not a separate row: {dump}"
    );
    assert!(
        denied.label.contains("deny by shammah"),
        "the denial's row must say who denied it, on the row itself: {dump}"
    );
}

/// Issue #1426: #439's fold-in only reached a row created by
/// `project_remote_brain_run_event`'s own `RemoteBrainRunProjection` -- the
/// row keyed by `tool_rows`/correlated through `locally_rendered_tool_ids`.
/// A plain home-session tool call needing approval (the single most common
/// scenario: `?tools require confirmation`, an ordinary interactive `write`)
/// is never drawn through that function at all. It is drawn directly by
/// `dispatch_tool_uses`'s `active_tool_uses` row and completed by
/// `EventLoop::handle_tool_result` (tools.rs) -- a completely different
/// mechanism #439's own regression test never exercised, since that test
/// fed events straight into `project_remote_brain_run_event`.
///
/// This test drives the real production boundary instead: a real
/// `ConversationHistory::stage_assistant` round, `EventLoop::
/// handle_tool_approval_request` (opens the real dialog), `EventLoop::
/// resolve_dialog_result` (answers it, exactly as `DialogResult` delivery
/// does in production), and `EventLoop::handle_tool_result` (completes the
/// row, exactly as the real tool-execution task's `ReplEvent::ToolResult`
/// does). Before the fix, the decision is computed and sent to unblock the
/// tool but never reaches the row at all; the row's own rendered content
/// carries no trace of who approved it or how.
#[tokio::test]
async fn home_session_tool_approval_decision_renders_on_its_own_row_not_nowhere() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            // Default mode is Normal -- a real dialog, no AutoAccept short-circuit.

            let query_id = uuid::Uuid::new_v4();
            let tool_use = crate::tools::ToolUse::new(
                "write".to_string(),
                serde_json::json!({"file_path": "notes.txt", "content": "hello"}),
            );
            let tool_id = tool_use.id.clone();

            // Stage the real conversation round the way `dispatch_tool_uses`
            // does, so `handle_tool_result`'s own `record_tool_result` call
            // succeeds instead of hitting the "discarded after closed tool
            // round" fallback.
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
                            name: tool_use.name.clone(),
                            input: tool_use.input.clone(),
                        }],
                    },
                )
                .expect("stage the real tool round");

            // Draw the real row the way `dispatch_tool_uses` does: one
            // WorkUnit shared by every tool call in the turn, one row for
            // this call, tracked in `active_tool_uses` by tool_id.
            use crate::cli::repl_event::tool_display::format_tool_label;
            let work_unit = event_loop.output_manager.start_work_unit("Tools");
            let row_idx = work_unit.add_row(format_tool_label(&tool_use.name, &tool_use.input));
            event_loop.active_tool_uses.write().await.insert(
                tool_id.clone(),
                (
                    tool_use.name.clone(),
                    tool_use.input.clone(),
                    Arc::clone(&work_unit),
                    row_idx,
                ),
            );

            // Request approval -- opens the real dialog (Normal mode).
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            event_loop
                .handle_tool_approval_request(query_id, tool_use.clone(), Vec::new(), response_tx)
                .await
                .expect("tool approval request must be accepted");
            assert!(
                event_loop.tui_renderer.lock().await.active_dialog.is_some(),
                "Normal mode must show a real approval dialog for this write"
            );

            // Answer it: "1. Yes" (index 0), exactly as a real user approving
            // once from the compact dialog would.
            event_loop
                .resolve_dialog_result(crate::cli::tui::DialogResult::Selected(0))
                .await
                .expect("resolving the dialog result must succeed");

            let confirmation = response_rx
                .await
                .expect("the tool execution task must receive the real confirmation");
            assert!(
                matches!(
                    confirmation,
                    crate::cli::repl_event::events::ConfirmationResult::ApproveOnce
                ),
                "expected ApproveOnce for a plain 'Yes'; got {confirmation:?}"
            );

            // Complete the row exactly as the real tool-execution task's
            // `ReplEvent::ToolResult` handling does.
            event_loop
                .handle_tool_result(
                    query_id,
                    round_token,
                    tool_id.clone(),
                    Ok("wrote 5 bytes to notes.txt".to_string()),
                )
                .await
                .expect("handle_tool_result must succeed");

            assert!(
                !event_loop
                    .active_tool_uses
                    .read()
                    .await
                    .contains_key(&tool_id),
                "the completed row must be removed from active_tool_uses"
            );

            let projected = crate::cli::test_projection::try_project_for_test(
                work_unit.as_ref(),
                &crate::theme::ColorScheme::default(),
            )
            .unwrap();
            let dump = format!("{projected:#?}");

            // Exactly one row for this call -- no second, separately
            // correlated "approval <tool_id>" row anywhere (the pre-#439,
            // and still-current-for-this-path, shape #1426 reports).
            assert_eq!(
                projected.children.len(),
                1,
                "the write call must be the only row in its Tools group, no \
                 separate approval row: {dump}"
            );
            assert!(
                !projected
                    .children
                    .iter()
                    .any(|row| row.label.starts_with("approval")),
                "an approval must not render as its own row, separately \
                 correlated by id to the tool call it gates: {dump}"
            );

            // The call's own row must carry who approved it and how.
            let call_row = &projected.children[0];
            assert!(
                call_row.label.contains("write"),
                "the only row must be the write call itself: {dump}"
            );
            let call_body: String = call_row
                .children
                .iter()
                .flat_map(|child| child.body.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let expected_decision = format!("approve_once by {}", event_loop.participant_subject);
            assert!(
                call_body.contains(&expected_decision),
                "the write call's own row must show who approved it and how \
                 ({expected_decision:?}): {dump}"
            );
        })
        .await;
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

    // DEFECT REGRESSION (#1492): A locally initiated run's subsequent RunStatusChanged
    // event must not project a Brain run UUID group.
    let mut local_completed = brain_event(
        4,
        "daemon",
        BrainEventKind::RunStatusChanged {
            run_id: local_run_id,
            status: BrainRunStatus::Completed,
            detail: None,
        },
    );
    local_completed.run_id = Some(local_run_id);
    let mut say_projected = std::collections::HashSet::new();
    let mut local_projections = std::collections::VecDeque::new();
    assert!(super::project_remote_brain_live_run_event(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &local_started,
        &super::LocallyRenderedRuns::default(),
        &mut say_projected,
        Some(my_attachment),
    ));
    assert!(
        say_projected.contains(&local_run_id),
        "locally initiated run must be recorded in say_projected"
    );
    let locally_rendered = super::LocallyRenderedRuns {
        say_completed: say_projected.clone(),
        in_flight: std::collections::HashSet::new(),
    };
    assert!(super::project_remote_brain_live_run_event(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &local_completed,
        &locally_rendered,
        &mut say_projected,
        Some(my_attachment),
    ));
    assert!(
        projections.get(&local_run_id).is_none(),
        "INVARIANT: a locally initiated run's RunStatusChanged paints no run group (#1492)"
    );
}

/// DEFECT REGRESSION (#1492): A tool-bearing delegated turn suppresses its
/// terminal RunStatusChanged event so no Brain run UUID row renders beside it.
#[tokio::test]
async fn delegated_tool_bearing_turn_does_not_paint_run_group_from_daemon_events() {
    use crate::brain::{BrainEventKind, BrainRunStatus, RunId};
    let output = replay_output_manager();
    let mut projections = std::collections::HashMap::new();
    let mut local_projections = std::collections::VecDeque::new();
    let mut say_projected = std::collections::HashSet::new();

    let run_id = RunId(uuid::Uuid::new_v4());
    let mut tool_ids = std::collections::HashSet::new();
    tool_ids.insert("tool-1".to_string());
    local_projections.push_back(super::LocalBrainProjection {
        run_id,
        source: "(bash \"echo hi\")".into(),
        output: "hi\n".into(),
        tool_ids,
        approval_ids: std::collections::HashSet::new(),
        program_seq: Some(1),
        transient_output_unit: None,
        failed: false,
    });

    let mut result_event = brain_event(
        2,
        "daemon",
        BrainEventKind::Result {
            request_seq: 1,
            output: "hi\n".into(),
            error: None,
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
    );
    result_event.run_id = Some(run_id);

    let locally_rendered = super::LocallyRenderedRuns {
        say_completed: say_projected.clone(),
        in_flight: [run_id].into_iter().collect(),
    };

    assert!(super::project_remote_brain_live_run_event(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &result_event,
        &locally_rendered,
        &mut say_projected,
        None,
    ));
    assert!(
        say_projected.contains(&run_id),
        "tool-bearing turn must mark say_projected on SuppressAndComplete"
    );

    let mut status_event = brain_event(
        3,
        "daemon",
        BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Completed,
            detail: None,
        },
    );
    status_event.run_id = Some(run_id);

    let locally_rendered_after = super::LocallyRenderedRuns {
        say_completed: say_projected.clone(),
        in_flight: std::collections::HashSet::new(),
    };

    assert!(super::project_remote_brain_live_run_event(
        &output,
        &mut projections,
        &mut local_projections,
        true,
        &status_event,
        &locally_rendered_after,
        &mut say_projected,
        None,
    ));
    assert!(
        projections.get(&run_id).is_none(),
        "INVARIANT: tool-bearing turn must not project Brain run UUID group (#1492)"
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

/// #978/#422 control regression: a genuinely remote Interactive run still
/// projects its semantic turn even while a local projection exists for a
/// different run.
#[tokio::test]
async fn remote_run_events_still_paint_group_rows_beside_local_projections() {
    use crate::brain::{AttachmentId, BrainEventKind, BrainRun, BrainRunKind, BrainRunStatus};
    use crate::cli::messages::Message;

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
        .complete_transcript(&crate::theme::ColorScheme::default());
    for expected in ["Lisp program", source, greeting] {
        assert!(
            rendered.contains(expected),
            "INVARIANT: a genuinely remote Brain turn keeps its semantic transcript; \
             missing {expected:?}; rendered=\n{rendered}"
        );
    }
    assert!(
        !rendered.contains("Interactive run")
            && !rendered.contains(&remote_run_id.0.to_string())
            && !rendered.contains("result —"),
        "INVARIANT: remote Interactive output is semantic, not UUID lifecycle chrome; \
         rendered=\n{rendered}"
    );
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

#[test]
fn test_provider_profile_resolves_by_case_insensitive_prefix() {
    let profiles = vec![
        claude_profile("ChatGPT Personal", "gpt-5"),
        claude_profile("deep", "claude-opus"),
    ];
    assert_eq!(
        resolve_provider_profile(&profiles, "chatgpt"),
        Ok(0),
        "a lowercase prefix of a multi-word name must resolve without typing the name in full"
    );
    assert_eq!(
        resolve_provider_profile(&profiles, "ChatGPT"),
        Ok(0),
        "prefix matching is case-insensitive, matching exact-name resolution"
    );
    assert_eq!(
        resolve_provider_profile(&profiles, "d"),
        Ok(1),
        "a single-character prefix still resolves when it is unambiguous"
    );
}

#[test]
fn test_provider_profile_prefix_shared_by_two_entries_is_ambiguous() {
    let profiles = vec![
        claude_profile("ChatGPT Personal", "gpt-5"),
        claude_profile("ChatGPT Work", "gpt-5"),
    ];
    assert!(
        resolve_provider_profile(&profiles, "chatgpt")
            .unwrap_err()
            .contains("ambiguous"),
        "a prefix matching more than one profile must fail closed, not silently pick one"
    );
    assert_eq!(
        resolve_provider_profile(&profiles, "chatgpt personal"),
        Ok(0),
        "a longer, disambiguating prefix (or the exact name) still resolves"
    );
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

// ── #427 item 3: Structured patterns instead of bash:* ────────────────────

#[test]
fn test_dialog_result_bash_flag_shape_mints_structured_pattern_not_wildcard() {
    // The issue's own example: `gh issue create --repo <arg> --title <arg>
    // --body <arg>` — a single invocation with a fixed subcommand skeleton
    // and only flag *values* varying. Both the session (1) and persistent
    // (2) dialog choices must mint a Structured pattern for this shape
    // instead of the blanket `bash:*` wildcard.
    let tool = make_tool_use(
        "bash",
        serde_json::json!({"command": "gh issue create --repo owner/repo --title fix --body details"}),
    );

    let session = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(1), &tool);
    match session {
        crate::cli::repl_event::events::ConfirmationResult::ApprovePatternSession(p) => {
            assert_eq!(
                p.pattern_type,
                crate::tools::PatternType::Structured,
                "qualifying bash command must mint Structured, not Wildcard; pattern={:?}",
                p
            );
            assert_eq!(p.command_pattern.as_deref(), Some("gh"));
            assert_eq!(
                p.args_pattern.as_deref(),
                Some("issue create --repo * --title * --body *")
            );
        }
        other => panic!("expected ApprovePatternSession, got {:?}", other),
    }

    let persistent =
        dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(2), &tool);
    match persistent {
        crate::cli::repl_event::events::ConfirmationResult::ApprovePatternPersistent(p) => {
            assert_eq!(
                p.pattern_type,
                crate::tools::PatternType::Structured,
                "qualifying bash command must mint Structured, not Wildcard; pattern={:?}",
                p
            );
            assert_eq!(p.command_pattern.as_deref(), Some("gh"));
            assert_eq!(
                p.args_pattern.as_deref(),
                Some("issue create --repo * --title * --body *")
            );
        }
        other => panic!("expected ApprovePatternPersistent, got {:?}", other),
    }
}

#[test]
fn test_dialog_result_bash_positional_shape_falls_back_to_wildcard() {
    // `cp <src> <dst>` — bare positional arguments, not `--flag value`
    // pairs — is exactly the shape #427/#429 call unsafe to templatize
    // (the wildcarded slot could be a filesystem path with no containment
    // check). Must fall back to the pre-existing `tool:*` wildcard, not a
    // new, less-safe behaviour, and never silently drop the approval.
    let tool = make_tool_use(
        "bash",
        serde_json::json!({"command": "cp ./target/out.txt /tmp/backup.txt"}),
    );
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(2), &tool);
    match result {
        crate::cli::repl_event::events::ConfirmationResult::ApprovePatternPersistent(p) => {
            assert_eq!(
                p.pattern_type,
                crate::tools::PatternType::Wildcard,
                "a bare-positional bash command must fall back to Wildcard, not \
                 templatize an unconstrained path slot; pattern={:?}",
                p
            );
            assert_eq!(p.pattern, "*");
        }
        other => panic!("expected ApprovePatternPersistent, got {:?}", other),
    }
}

#[test]
fn test_dialog_result_bash_short_flag_does_not_absorb_a_path_value() {
    // `rm -v <path>` must not be misread as "`-v` takes a value" and
    // wildcard the path — short flags are always boolean/literal here.
    // The stray positional after `-v` must trigger the same safe fallback
    // as the bare-positional case.
    let tool = make_tool_use("bash", serde_json::json!({"command": "rm -v /etc/passwd"}));
    let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(2), &tool);
    match result {
        crate::cli::repl_event::events::ConfirmationResult::ApprovePatternPersistent(p) => {
            assert_eq!(
                p.pattern_type,
                crate::tools::PatternType::Wildcard,
                "a short flag must never absorb a following path as its value; \
                 pattern={:?}",
                p
            );
        }
        other => panic!("expected ApprovePatternPersistent, got {:?}", other),
    }
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

async fn assert_clear_command_resets_provider_context(command: &str) {
    use crate::cli::conversation::ToolRoundError;
    use crate::providers::{ContentBlock, Message};

    let (mut event_loop, output) = lifecycle_test_event_loop();
    output.disable_stdout();
    let query_id = uuid::Uuid::new_v4();
    let round_token = {
        let mut conversation = event_loop.conversation.write().await;
        conversation.add_user_message("history before clear".to_string());
        conversation.add_assistant_message("answer before clear".to_string());
        conversation
            .stage_assistant(
                query_id,
                Message {
                    role: "assistant".into(),
                    content: vec![ContentBlock::ToolUse {
                        id: "call_before_clear".into(),
                        name: "Read".into(),
                        input: serde_json::json!({"path": "README.md"}),
                    }],
                },
            )
            .expect("the fixture must stage the provider-invisible tool round")
    };
    let mut observed_rx = observe_llm_queries(&mut event_loop);

    event_loop
        .handle_user_input(command.to_string())
        .await
        .expect("the advertised clear command must dispatch through the active event loop");

    let mut conversation = event_loop.conversation.write().await;
    assert!(
        conversation.get_messages().is_empty(),
        "{command} must remove every committed provider-visible message; messages={:?}",
        conversation.get_messages()
    );
    assert_eq!(
        conversation.record_tool_result(
            query_id,
            round_token,
            "call_before_clear",
            &Ok("stale result".to_string()),
        ),
        Err(ToolRoundError::NoActiveStage),
        "{command} must remove provider-invisible staged tool rounds as part of the same conversation-owned clear boundary"
    );
    drop(conversation);

    let transcript = output
        .get_messages()
        .iter()
        .map(|message| message.format(&crate::theme::ColorScheme::default()))
        .collect::<Vec<_>>();
    assert!(
        transcript
            .iter()
            .any(|message| message.contains("Conversation history cleared. Starting fresh.")),
        "{command} must visibly confirm the fresh context; transcript={transcript:?}"
    );
    assert!(
        transcript
            .iter()
            .all(|message| !message.contains("recognized but not yet implemented")),
        "{command} must never reach the generic command fallback; transcript={transcript:?}"
    );

    event_loop
        .handle_user_input("fresh question".to_string())
        .await
        .expect("the first post-clear query must dispatch");
    let (_, text, request_messages) =
        tokio::time::timeout(std::time::Duration::from_secs(3), observed_rx.recv())
            .await
            .expect("the first post-clear provider request must not stall")
            .expect("the provider request observer must remain open");
    assert_eq!(
        text, "fresh question",
        "the provider request must carry the post-clear query, not prior text"
    );
    assert_eq!(
        request_messages,
        vec![Message::user("fresh question")],
        "the first provider request after {command} must contain only the fresh user turn; request_messages={request_messages:?}"
    );
}

struct SummaryRequestRecorder {
    requests: std::sync::Mutex<Vec<Vec<crate::providers::Message>>>,
    request_ready: tokio::sync::Notify,
}

impl SummaryRequestRecorder {
    fn new() -> Self {
        Self {
            requests: std::sync::Mutex::new(Vec::new()),
            request_ready: tokio::sync::Notify::new(),
        }
    }

    async fn request_containing_user_text(&self, expected: &str) -> Vec<crate::providers::Message> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(request) = self
                    .requests
                    .lock()
                    .expect("summary request recorder lock poisoned")
                    .iter()
                    .find(|request| {
                        request.iter().any(|message| {
                            message.role == "user" && message.text_content() == expected
                        })
                    })
                    .cloned()
                {
                    return request;
                }
                self.request_ready.notified().await;
            }
        })
        .await
        .expect("the real LlmLoop must reach the generator request boundary")
    }
}

#[async_trait::async_trait]
impl crate::generators::Generator for SummaryRequestRecorder {
    async fn generate(
        &self,
        messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<crate::generators::GeneratorResponse> {
        let is_summary_request = messages.iter().any(|message| {
            message
                .text_content()
                .starts_with("Summarise the following conversation history concisely")
        });
        self.requests
            .lock()
            .expect("summary request recorder lock poisoned")
            .push(messages);
        self.request_ready.notify_waiters();
        let text = if is_summary_request {
            "fresh post-clear summary"
        } else {
            "(say \"fresh response\")"
        };
        Ok(crate::generators::GeneratorResponse {
            text: text.to_string(),
            content_blocks: vec![crate::providers::ContentBlock::text(text)],
            tool_uses: Vec::new(),
            metadata: crate::generators::ResponseMetadata {
                generator: "summary-request-recorder".to_string(),
                model: "summary-request-recorder".to_string(),
                confidence: None,
                stop_reason: None,
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
                max_context_messages: Some(4),
            };
        &CAPABILITIES
    }

    fn name(&self) -> &str {
        "summary-request-recorder"
    }
}

struct BlockingSummaryRequestRecorder {
    requests: std::sync::Mutex<Vec<Vec<crate::providers::Message>>>,
    summary_started: tokio::sync::Notify,
    release_summary: tokio::sync::Notify,
    summary_returned: tokio::sync::Notify,
    newer_request_started: tokio::sync::Notify,
    release_newer_request: tokio::sync::Notify,
}

#[derive(Clone, Copy, Debug)]
enum LateProviderOutcome {
    Success,
    Failure,
}

struct BlockingLateProvider {
    outcome: LateProviderOutcome,
    old_started: tokio::sync::Notify,
    release_old: tokio::sync::Notify,
    newer_started: tokio::sync::Notify,
    release_newer: tokio::sync::Notify,
}

impl BlockingLateProvider {
    fn new(outcome: LateProviderOutcome) -> Self {
        Self {
            outcome,
            old_started: tokio::sync::Notify::new(),
            release_old: tokio::sync::Notify::new(),
            newer_started: tokio::sync::Notify::new(),
            release_newer: tokio::sync::Notify::new(),
        }
    }

    fn response(text: &str) -> crate::generators::GeneratorResponse {
        crate::generators::GeneratorResponse {
            text: text.into(),
            content_blocks: vec![crate::providers::ContentBlock::text(text)],
            tool_uses: Vec::new(),
            metadata: crate::generators::ResponseMetadata {
                generator: "blocking-late-provider".into(),
                model: "blocking-late-provider-model".into(),
                confidence: None,
                stop_reason: None,
                input_tokens: Some(3),
                output_tokens: Some(4),
                latency_ms: Some(5),
                primary_allowance_used_percent: Some(6.0),
                secondary_allowance_used_percent: Some(7.0),
            },
        }
    }
}

#[async_trait::async_trait]
impl crate::generators::Generator for BlockingLateProvider {
    async fn generate(
        &self,
        messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<crate::generators::GeneratorResponse> {
        let query = messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(crate::providers::Message::text_content)
            .unwrap_or_default();
        match query.as_str() {
            "old provider request" => {
                self.old_started.notify_one();
                self.release_old.notified().await;
                match self.outcome {
                    LateProviderOutcome::Success => {
                        let tool = crate::tools::ToolUse {
                            id: "late-provider-tool".into(),
                            name: "late_probe".into(),
                            input: serde_json::json!({}),
                        };
                        let mut response = Self::response("(say \"stale provider text\")");
                        response
                            .content_blocks
                            .push(crate::providers::ContentBlock::ToolUse {
                                id: tool.id.clone(),
                                name: tool.name.clone(),
                                input: tool.input.clone(),
                            });
                        response.tool_uses.push(tool);
                        response.metadata.input_tokens = Some(901);
                        response.metadata.output_tokens = Some(902);
                        Ok(response)
                    }
                    LateProviderOutcome::Failure => {
                        anyhow::bail!("stale provider failure after reset")
                    }
                }
            }
            "new active request" => {
                self.newer_started.notify_one();
                self.release_newer.notified().await;
                Ok(Self::response("(say \"new active response\")"))
            }
            _ => Ok(Self::response("(say \"fresh provider response\")")),
        }
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
                max_context_messages: None,
            };
        &CAPABILITIES
    }

    fn name(&self) -> &str {
        "blocking-late-provider"
    }
}

struct LateProviderProbe {
    executions: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::tools::Tool for LateProviderProbe {
    fn name(&self) -> &str {
        "late_probe"
    }

    fn effect(&self) -> finch_programs::ExecutionEffect {
        finch_programs::ExecutionEffect::WorkspaceRead
    }

    fn description(&self) -> &str {
        "records whether stale provider output launched a tool"
    }

    fn input_schema(&self) -> crate::tools::ToolInputSchema {
        crate::tools::ToolInputSchema::simple(Vec::new())
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _context: &crate::tools::ToolContext<'_>,
    ) -> anyhow::Result<String> {
        self.executions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok("stale tool executed".into())
    }
}

impl BlockingSummaryRequestRecorder {
    fn new() -> Self {
        Self {
            requests: std::sync::Mutex::new(Vec::new()),
            summary_started: tokio::sync::Notify::new(),
            release_summary: tokio::sync::Notify::new(),
            summary_returned: tokio::sync::Notify::new(),
            newer_request_started: tokio::sync::Notify::new(),
            release_newer_request: tokio::sync::Notify::new(),
        }
    }

    fn requests(&self) -> Vec<Vec<crate::providers::Message>> {
        self.requests
            .lock()
            .expect("blocking summary recorder lock poisoned")
            .clone()
    }
}

#[async_trait::async_trait]
impl crate::generators::Generator for BlockingSummaryRequestRecorder {
    async fn generate(
        &self,
        messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<crate::generators::GeneratorResponse> {
        let is_summary_request = messages.iter().any(|message| {
            message
                .text_content()
                .starts_with("Summarise the following conversation history concisely")
        });
        let is_newer_request = messages.iter().any(|message| {
            message.role == "user"
                && message.text_content() == "new active prompt before old invalidation arrives"
        });
        self.requests
            .lock()
            .expect("blocking summary recorder lock poisoned")
            .push(messages);
        if is_summary_request {
            self.summary_started.notify_one();
            self.release_summary.notified().await;
            self.summary_returned.notify_one();
        }
        if is_newer_request {
            self.newer_request_started.notify_one();
            self.release_newer_request.notified().await;
        }
        let text = if is_summary_request {
            "summary produced from the cleared conversation"
        } else {
            "(say \"fresh provider response\")"
        };
        Ok(crate::generators::GeneratorResponse {
            text: text.to_string(),
            content_blocks: vec![crate::providers::ContentBlock::text(text)],
            tool_uses: Vec::new(),
            metadata: crate::generators::ResponseMetadata {
                generator: "blocking-summary-recorder".to_string(),
                model: "blocking-summary-recorder".to_string(),
                confidence: None,
                stop_reason: None,
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
                max_context_messages: Some(4),
            };
        &CAPABILITIES
    }

    fn name(&self) -> &str {
        "blocking-summary-recorder"
    }
}

async fn assert_clear_during_inflight_summary_cancels_stale_request(command: &str) {
    use crate::cli::repl_event::query_state::QueryState;

    let recorder = Arc::new(BlockingSummaryRequestRecorder::new());
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    let patterns = tempfile::tempdir()
        .expect("isolated in-flight summary-reset tool state")
        .path()
        .join("patterns.json");
    let executor = crate::tools::ToolExecutor::new(
        crate::tools::ToolRegistry::new(),
        crate::tools::PermissionManager::new(),
        patterns,
    )
    .expect("construct inert in-flight summary-reset tool executor");
    let mut event_loop = EventLoop::new_named_brain_test_runner(
        Arc::clone(&recorder) as Arc<dyn crate::generators::Generator>,
        Vec::new(),
        Arc::new(tokio::sync::Mutex::new(executor)),
        runtime,
    );
    event_loop.output_manager.disable_stdout();
    event_loop.max_verbatim_messages = 4;
    event_loop.enable_summarization = true;
    {
        let mut conversation = event_loop.conversation.write().await;
        for message in summary_reuse_fixture("pre-clear") {
            conversation.add_message(message);
        }
    }

    event_loop.start_llm_worker();
    event_loop
        .handle_user_input("question whose summary is blocked".to_string())
        .await
        .expect("the real LlmLoop query must start");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        recorder.summary_started.notified(),
    )
    .await
    .expect("the real LlmLoop must reach the blocking summarizer");
    let query_id = event_loop
        .active_query_id
        .read()
        .await
        .expect("the blocked summary must belong to the active query");
    let stale_round = event_loop
        .conversation
        .write()
        .await
        .stage_assistant(
            query_id,
            crate::providers::Message {
                role: "assistant".into(),
                content: vec![crate::providers::ContentBlock::ToolUse {
                    id: "late_old_tool".into(),
                    name: "Read".into(),
                    input: serde_json::json!({"path": "README.md"}),
                }],
            },
        )
        .expect("the hostile fixture must retain an old-generation tool token");

    event_loop
        .handle_user_input(command.to_string())
        .await
        .expect("the reset command must invalidate the in-flight request");
    assert!(
        matches!(
            event_loop.query_states.get_state(query_id).await,
            Some(QueryState::Cancelled)
        ),
        "{command} must atomically cancel the query whose provider context it invalidated"
    );
    assert!(
        event_loop
            .conversation
            .read()
            .await
            .get_messages()
            .is_empty(),
        "{command} must clear the conversation while the summarizer is blocked"
    );
    assert_eq!(
        *event_loop.active_query_id.read().await,
        None,
        "{command} must release the cancelled query before visibly confirming the fresh context"
    );

    const FRESH_PROMPT: &str = "fresh prompt submitted while old summary remains blocked";
    event_loop
        .handle_user_input(FRESH_PROMPT.to_string())
        .await
        .expect("the first post-reset prompt must start immediately");
    let fresh_query_id = event_loop
        .active_query_id
        .read()
        .await
        .expect("the first post-reset prompt must own the active slot");
    assert_ne!(
        fresh_query_id, query_id,
        "the fresh prompt must not reuse the cancelled query identity"
    );
    let fresh_request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(request) = recorder.requests().into_iter().find(|request| {
                request
                    .iter()
                    .any(|message| message.role == "user" && message.text_content() == FRESH_PROMPT)
            }) {
                break request;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the fresh prompt must reach the real provider while the old summarizer is blocked");
    assert!(
        fresh_request.iter().all(|message| {
            !message.text_content().contains("pre-clear")
                && !message
                    .text_content()
                    .contains("summary produced from the cleared conversation")
        }),
        "{command} must send only fresh-generation context; request={fresh_request:?}"
    );

    loop {
        let event = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            event_loop.event_rx.recv(),
        )
        .await
        .expect("the fresh prompt must reach a terminal event while the old summary is blocked")
        .expect("the event channel must remain open");
        assert!(
            !matches!(event, ReplEvent::QueryContextInvalidated { query_id: id } if id == query_id),
            "the old query cannot invalidate before its summarizer is released"
        );
        let fresh_complete = matches!(event, ReplEvent::StreamingComplete { query_id: id, .. } if id == fresh_query_id);
        event_loop
            .handle_event(event)
            .await
            .expect("fresh-generation events must dispatch");
        if fresh_complete {
            break;
        }
    }
    assert_eq!(
        *event_loop.active_query_id.read().await,
        None,
        "the fresh prompt must settle normally before the old task returns"
    );
    assert_eq!(
        recorder
            .requests()
            .iter()
            .filter(|request| request.iter().any(|message| {
                message.role == "user" && message.text_content() == FRESH_PROMPT
            }))
            .count(),
        1,
        "the first post-reset prompt must reach the provider exactly once"
    );

    const NEW_ACTIVE: &str = "new active prompt before old invalidation arrives";
    const NEW_QUEUED: &str = "queued prompt that late invalidation must preserve";
    event_loop
        .handle_user_input(NEW_ACTIVE.to_string())
        .await
        .expect("a newer query must start after the fresh prompt settles");
    let new_active_id = event_loop
        .active_query_id
        .read()
        .await
        .expect("the newer query must own the active slot");
    event_loop
        .handle_user_input(NEW_QUEUED.to_string())
        .await
        .expect("a subsequent prompt must queue behind the newer query");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        recorder.newer_request_started.notified(),
    )
    .await
    .expect("the newer active query must block at the provider boundary");
    assert_eq!(
        event_loop
            .pending_queries
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<Vec<_>>(),
        [NEW_QUEUED],
        "the hostile fixture must contain a newer queued prompt"
    );
    {
        let tui = event_loop.tui_renderer.lock().await;
        tui.set_operation_status("fresh generation remains active");
    }
    use crate::cli::tui::TuiStatusPort;
    let status_before_late_event = event_loop.status_bar.status_without_session();
    let transcript_before_late_event = event_loop
        .output_manager
        .get_messages()
        .iter()
        .map(|message| message.format(&crate::theme::ColorScheme::default()))
        .collect::<Vec<_>>();
    let requests_before_late_event = recorder.requests().len();

    recorder.release_summary.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        recorder.summary_returned.notified(),
    )
    .await
    .expect("the released summarizer must return");
    let terminal = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        event_loop.event_rx.recv(),
    )
    .await
    .expect("the invalidated old request must emit one terminal event")
    .expect("the event channel must remain open");
    assert!(
        matches!(terminal, ReplEvent::QueryContextInvalidated { query_id: id } if id == query_id),
        "the blocked newer request cannot produce an event before the old generation's invalidation; got {terminal:?}"
    );
    event_loop
        .handle_event(terminal)
        .await
        .expect("the invalidation terminal event must dispatch");

    assert_eq!(
        *event_loop.active_query_id.read().await,
        Some(new_active_id),
        "{command} late invalidation must not release the newer active query"
    );
    assert_eq!(
        event_loop
            .pending_queries
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<Vec<_>>(),
        [NEW_QUEUED],
        "{command} late invalidation must not delete newer queued prompts"
    );
    assert_eq!(
        event_loop.status_bar.status_without_session(),
        status_before_late_event,
        "{command} late invalidation must not overwrite newer-generation status"
    );
    assert_eq!(
        event_loop
            .output_manager
            .get_messages()
            .iter()
            .map(|message| message.format(&crate::theme::ColorScheme::default()))
            .collect::<Vec<_>>(),
        transcript_before_late_event,
        "{command} late invalidation must not append or rewrite visible output"
    );
    let requests = recorder.requests();
    assert!(
        requests.len() >= requests_before_late_event,
        "request recording is append-only across the late event; before={requests_before_late_event}, requests={requests:?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.iter().any(|message| {
                message.role == "user"
                    && message.text_content() == "question whose summary is blocked"
            }))
            .count(),
        0,
        "{command} must never send the invalidated old query to the main provider; requests={requests:?}"
    );

    event_loop
        .handle_event(ReplEvent::ToolResult {
            query_id,
            round_token: stale_round,
            tool_id: "late_old_tool".into(),
            result: Ok("late old result".into()),
        })
        .await
        .expect("a late old-generation tool event must be ignored idempotently");
    assert_eq!(
        *event_loop.active_query_id.read().await,
        Some(new_active_id),
        "a late old-generation tool event must not mutate the newer active query"
    );
    assert_eq!(
        event_loop
            .pending_queries
            .front()
            .map(|(text, _, _)| text.as_str()),
        Some(NEW_QUEUED),
        "a late old-generation tool event must preserve newer queued input"
    );
    event_loop
        .handle_event(ReplEvent::StreamingComplete {
            query_id,
            full_response: "late old completion".into(),
        })
        .await
        .expect("a late old-generation completion must be ignored idempotently");
    assert_eq!(
        *event_loop.active_query_id.read().await,
        Some(new_active_id),
        "late old-generation tool and completion events must preserve the newer active query"
    );
    assert_eq!(
        event_loop
            .pending_queries
            .front()
            .map(|(text, _, _)| text.as_str()),
        Some(NEW_QUEUED),
        "late old-generation tool and completion events must preserve newer queued input"
    );
    assert_eq!(
        event_loop.status_bar.status_without_session(),
        status_before_late_event,
        "late old-generation tool and completion events must not overwrite newer-generation status"
    );
    assert_eq!(
        event_loop
            .output_manager
            .get_messages()
            .iter()
            .map(|message| message.format(&crate::theme::ColorScheme::default()))
            .collect::<Vec<_>>(),
        transcript_before_late_event,
        "late old-generation tool and completion events must not append or rewrite visible output"
    );

    let regrown = summary_reuse_fixture("post-clear");
    let fresh_compactor = crate::cli::conversation_compactor::ConversationCompactor::new(
        Arc::clone(&recorder) as Arc<dyn crate::generators::Generator>,
        Arc::clone(&event_loop.summary_cache),
    );
    assert!(
        matches!(fresh_compactor.plan_summary(&regrown, 4), crate::cli::conversation_compactor::SummaryPlan::Summarize { .. }),
        "{command} must reject the late summary commit; a regrown same-shape history must require fresh summarization"
    );
    tokio::task::yield_now().await;
    assert_eq!(
        recorder
            .requests()
            .iter()
            .filter(|request| request.iter().any(|message| {
                message.role == "user"
                    && message.text_content() == "question whose summary is blocked"
            }))
            .count(),
        0,
        "{command} must produce no late old-generation provider effect after terminal invalidation"
    );
    recorder.release_newer_request.notify_one();
}

async fn assert_reset_fences_late_main_provider_result(
    command: &str,
    outcome: LateProviderOutcome,
) {
    use crate::cli::repl_event::query_state::QueryState;
    use std::sync::atomic::Ordering;

    let provider = Arc::new(BlockingLateProvider::new(outcome));
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut registry = crate::tools::ToolRegistry::new();
    registry.register(Box::new(LateProviderProbe {
        executions: Arc::clone(&executions),
    }));
    let definitions = registry.definitions();
    let patterns = tempfile::tempdir()
        .expect("isolated late-provider tool state")
        .path()
        .join("patterns.json");
    let executor =
        crate::tools::ToolExecutor::new(registry, crate::tools::PermissionManager::new(), patterns)
            .expect("construct late-provider tool executor");
    let mut event_loop = EventLoop::new_named_brain_test_runner(
        Arc::clone(&provider) as Arc<dyn crate::generators::Generator>,
        definitions,
        Arc::new(tokio::sync::Mutex::new(executor)),
        Arc::new(crate::runtime::ProgramRuntime::new()),
    );
    event_loop.output_manager.disable_stdout();
    event_loop.start_llm_worker();

    event_loop
        .handle_user_input("old provider request".into())
        .await
        .expect("the old real provider request must start");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.old_started.notified(),
    )
    .await
    .expect("the old request must block inside the real provider");
    let old_query_id = event_loop
        .active_query_id
        .read()
        .await
        .expect("the blocked old provider request must own the active slot");

    event_loop
        .handle_user_input(command.into())
        .await
        .expect("the reset command must complete while the old provider is blocked");
    assert!(
        matches!(
            event_loop.query_states.get_state(old_query_id).await,
            Some(QueryState::Cancelled)
        ),
        "{command} must terminalize the blocked provider query before confirming"
    );
    assert_eq!(
        *event_loop.active_query_id.read().await,
        None,
        "{command} must release the old provider query before confirming"
    );

    const FRESH: &str = "fresh request after provider reset";
    event_loop
        .handle_user_input(FRESH.into())
        .await
        .expect("the first fresh query must start immediately");
    let fresh_query_id = event_loop
        .active_query_id
        .read()
        .await
        .expect("the first fresh query must own the released slot");
    loop {
        let event = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            event_loop.event_rx.recv(),
        )
        .await
        .expect("the fresh query must settle while the old provider stays blocked")
        .expect("the event channel must remain open");
        let complete = matches!(event, ReplEvent::StreamingComplete { query_id, .. } if query_id == fresh_query_id);
        event_loop
            .handle_event(event)
            .await
            .expect("fresh-query events must dispatch");
        if complete {
            break;
        }
    }
    assert_eq!(
        *event_loop.active_query_id.read().await,
        None,
        "the fresh query must settle before the old provider returns"
    );

    const NEW_ACTIVE: &str = "new active request";
    const NEW_QUEUED: &str = "new queued request";
    event_loop
        .handle_user_input(NEW_ACTIVE.into())
        .await
        .expect("a newer query must start after the fresh turn settles");
    let new_query_id = event_loop
        .active_query_id
        .read()
        .await
        .expect("the newer query must own the active slot");
    event_loop
        .handle_user_input(NEW_QUEUED.into())
        .await
        .expect("a following prompt must queue behind the newer query");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.newer_started.notified(),
    )
    .await
    .expect("the newer query must block at the provider boundary");
    {
        let tui = event_loop.tui_renderer.lock().await;
        tui.set_operation_status("new generation owns the frontend");
    }
    use crate::cli::tui::TuiStatusPort;
    let status_before = event_loop.status_bar.status_without_session();
    let transcript_before = event_loop
        .output_manager
        .get_messages()
        .iter()
        .map(|message| message.content())
        .collect::<Vec<_>>();
    let conversation_before = event_loop.conversation.read().await.get_messages();

    provider.release_old.notify_one();
    let terminal = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        event_loop.event_rx.recv(),
    )
    .await
    .expect("the late provider result must reach its fenced terminal boundary")
    .expect("the event channel must remain open");
    assert!(
        matches!(terminal, ReplEvent::QueryContextInvalidated { query_id } if query_id == old_query_id),
        "late {outcome:?} must become an idempotent invalidation, not a result-derived event; got {terminal:?}"
    );
    event_loop
        .handle_event(terminal)
        .await
        .expect("the old invalidation event must dispatch idempotently");

    assert!(
        matches!(
            event_loop.query_states.get_state(old_query_id).await,
            Some(QueryState::Cancelled)
        ),
        "late {outcome:?} must not overwrite the old query's cancelled terminal state"
    );
    assert!(
        event_loop
            .query_states
            .get_metadata(old_query_id)
            .await
            .is_some_and(|metadata| metadata.invocation_metadata.is_none()),
        "late {outcome:?} statistics must not be recorded after {command}"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "late {outcome:?} provider tools must never execute after {command}"
    );
    assert_eq!(
        *event_loop.active_query_id.read().await,
        Some(new_query_id),
        "late {outcome:?} must preserve the newer active owner"
    );
    assert_eq!(
        event_loop
            .pending_queries
            .front()
            .map(|(text, _, _)| text.as_str()),
        Some(NEW_QUEUED),
        "late {outcome:?} must preserve the newer queued prompt"
    );
    assert_eq!(
        event_loop.status_bar.status_without_session(),
        status_before,
        "late {outcome:?} must not overwrite newer-generation status"
    );
    assert_eq!(
        event_loop
            .output_manager
            .get_messages()
            .iter()
            .map(|message| message.content())
            .collect::<Vec<_>>(),
        transcript_before,
        "late {outcome:?} must not mutate the post-reset transcript"
    );
    assert_eq!(
        event_loop.conversation.read().await.get_messages(),
        conversation_before,
        "late {outcome:?} must not mutate provider-visible conversation history"
    );
    let cache_probe = crate::cli::conversation_compactor::ConversationCompactor::new(
        Arc::clone(&provider) as Arc<dyn crate::generators::Generator>,
        Arc::clone(&event_loop.summary_cache),
    );
    assert!(
        matches!(
            cache_probe.plan_summary(&summary_reuse_fixture("post-provider-reset"), 4),
            crate::cli::conversation_compactor::SummaryPlan::Summarize { .. }
        ),
        "late {outcome:?} must not repopulate the reset summary cache"
    );
    provider.release_newer.notify_one();
}

#[tokio::test]
async fn test_clear_and_reset_fence_late_non_streaming_provider_success_and_failure() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for command in ["/clear", "/reset"] {
                for outcome in [LateProviderOutcome::Success, LateProviderOutcome::Failure] {
                    assert_reset_fences_late_main_provider_result(command, outcome).await;
                }
            }
        })
        .await;
}

fn summary_reuse_fixture(prefix: &str) -> Vec<crate::providers::Message> {
    use crate::providers::Message;
    vec![
        Message::user(format!("{prefix} user zero")),
        Message::assistant(format!("{prefix} assistant one")),
        Message::user(format!("{prefix} user two")),
        Message::assistant(format!("{prefix} assistant three")),
        Message::user(format!("{prefix} user four")),
        Message::assistant("shared boundary message"),
    ]
}

async fn assert_clear_command_invalidates_committed_summary(command: &str) {
    const STALE_SUMMARY: &str = "STALE SUMMARY FROM CLEARED CONVERSATION";
    const FRESH_QUESTION: &str = "fresh question after summary reset";

    let recorder = Arc::new(SummaryRequestRecorder::new());
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    let patterns = tempfile::tempdir()
        .expect("isolated summary-reset tool state")
        .path()
        .join("patterns.json");
    let executor = crate::tools::ToolExecutor::new(
        crate::tools::ToolRegistry::new(),
        crate::tools::PermissionManager::new(),
        patterns,
    )
    .expect("construct inert summary-reset tool executor");
    let mut event_loop = EventLoop::new_named_brain_test_runner(
        Arc::clone(&recorder) as Arc<dyn crate::generators::Generator>,
        Vec::new(),
        Arc::new(tokio::sync::Mutex::new(executor)),
        runtime,
    );
    event_loop.output_manager.disable_stdout();
    event_loop.max_verbatim_messages = 4;
    event_loop.enable_summarization = true;

    let mut old_history = summary_reuse_fixture("old");
    old_history.push(crate::providers::Message::user("old trailing message"));
    {
        let mut conversation = event_loop.conversation.write().await;
        for message in old_history.iter().cloned() {
            conversation.add_message(message);
        }
    }
    let boundary = crate::cli::conversation_compactor::ConversationCompactor::boundary_fingerprint(
        &old_history,
        5,
    );
    crate::cli::conversation_compactor::ConversationCompactor::new(
        Arc::clone(&recorder) as Arc<dyn crate::generators::Generator>,
        Arc::clone(&event_loop.summary_cache),
    )
    .commit_summary(5, boundary, STALE_SUMMARY.to_string());

    event_loop
        .handle_user_input(command.to_string())
        .await
        .expect("the clear alias must dispatch before the worker starts");
    {
        let mut conversation = event_loop.conversation.write().await;
        for message in summary_reuse_fixture("new") {
            conversation.add_message(message);
        }
    }

    event_loop.start_llm_worker();
    event_loop
        .handle_user_input(FRESH_QUESTION.to_string())
        .await
        .expect("the post-clear question must dispatch through the real LlmLoop");
    let provider_request = recorder.request_containing_user_text(FRESH_QUESTION).await;
    let rendered_request = provider_request
        .iter()
        .map(crate::providers::Message::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !rendered_request.contains(STALE_SUMMARY),
        "{command} must invalidate committed summary bytes before a same-boundary history regrows; provider_request={provider_request:?}"
    );
    assert!(
        rendered_request.contains("fresh post-clear summary"),
        "the actual generator request must carry a newly assembled post-clear summary, proving the test traversed request assembly; provider_request={provider_request:?}"
    );
}

#[tokio::test]
async fn test_clear_and_reset_commands_remove_committed_and_staged_provider_context() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for command in ["/clear", "/reset"] {
                assert_clear_command_resets_provider_context(command).await;
            }
        })
        .await;
}

#[tokio::test]
async fn test_clear_and_reset_commands_invalidate_summary_before_actual_generator_request() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for command in ["/clear", "/reset"] {
                assert_clear_command_invalidates_committed_summary(command).await;
            }
        })
        .await;
}

#[tokio::test]
async fn test_clear_and_reset_during_inflight_summary_never_send_stale_provider_request() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for command in ["/clear", "/reset"] {
                assert_clear_during_inflight_summary_cancels_stale_request(command).await;
            }
        })
        .await;
}

#[tokio::test]
async fn test_unrelated_help_command_preserves_provider_context_and_staged_round() {
    use crate::cli::conversation::ToolRoundProgress;
    use crate::providers::{ContentBlock, Message};

    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let query_id = uuid::Uuid::new_v4();
            let round_token = {
                let mut conversation = event_loop.conversation.write().await;
                conversation.add_user_message("history before help".to_string());
                conversation.add_assistant_message("answer before help".to_string());
                conversation
                    .stage_assistant(
                        query_id,
                        Message {
                            role: "assistant".into(),
                            content: vec![ContentBlock::ToolUse {
                                id: "call_before_help".into(),
                                name: "Read".into(),
                                input: serde_json::json!({"path": "README.md"}),
                            }],
                        },
                    )
                    .expect("the fixture must stage the provider-invisible tool round")
            };
            let retained_messages = event_loop.conversation.read().await.get_messages();
            let boundary =
                crate::cli::conversation_compactor::ConversationCompactor::boundary_fingerprint(
                    &retained_messages,
                    1,
                );
            crate::cli::conversation_compactor::ConversationCompactor::new(
                Arc::new(NeverCompletes),
                Arc::clone(&event_loop.summary_cache),
            )
            .commit_summary(1, boundary, "retained summary bytes".to_string());

            event_loop
                .handle_user_input("/help".to_string())
                .await
                .expect("the unrelated help command must retain its existing dispatch");

            let mut conversation = event_loop.conversation.write().await;
            assert_eq!(
                conversation.get_messages(),
                vec![
                    Message::user("history before help"),
                    Message::assistant("answer before help"),
                ],
                "an unrelated slash command must not clear committed provider context"
            );
            assert_eq!(
                conversation
                    .record_tool_result(
                        query_id,
                        round_token,
                        "call_before_help",
                        &Ok("retained result".to_string()),
                    )
                    .expect("the unrelated command must preserve the staged round"),
                ToolRoundProgress::Complete,
                "the staged round must remain usable after an unrelated command"
            );
            let compactor = crate::cli::conversation_compactor::ConversationCompactor::new(
                Arc::new(NeverCompletes),
                Arc::clone(&event_loop.summary_cache),
            );
            assert_eq!(
                compactor.plan_summary(&conversation.get_messages(), 1),
                crate::cli::conversation_compactor::SummaryPlan::Reuse(
                    "retained summary bytes".to_string()
                ),
                "an unrelated slash command must preserve the committed summary cache"
            );
        })
        .await;
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

/// Production-boundary regression for #363 ("Plan approval can wedge the
/// active query and suppress later turns"): drives the exact reported
/// scenario -- `/plan` -> `present_plan` -> "Approve and execute" -> its
/// execution continuation -> terminal result -> next user turn -- through
/// the real `dispatch_tool_uses` (query_processor.rs) and the real
/// `EventLoop::handle_event` pipeline, not a synthetic `ExecutingTools`
/// fixture.
///
/// This closes the gap between two narrower regressions that each cover half
/// of the reported wedge: `test_present_plan_dispatch_does_not_deadlock_on_its_own_mode_read_guard`
/// (query_processor.rs) proves `dispatch_tool_uses` no longer self-deadlocks
/// on the `ReplMode` lock when it calls `handle_present_plan` inline (#26,
/// landed on main as commit a71482f6 the night this test was written), and
/// `test_pending_user_message_folds_into_plan_approval_directive` above
/// proves `finalize_tool_execution`'s plan-approval fast path folds queued
/// text and keeps `active_query_id` correctly. Neither one starts from the
/// real dispatch entry point *and* carries the continuation through to a
/// terminal completion that must free the query and admit a queued turn --
/// exactly the "subsequent turns receive no GPT response" symptom #363
/// reported. Before #26's fix this test hangs (bounded by the timeout below)
/// at the same `mode.read().await` guard the isolated dispatch test catches;
/// after it, the approval, continuation, and next-turn admission all
/// terminalize and this test passes.
#[tokio::test]
async fn test_plan_approval_wedge_363_next_turn_is_admitted_after_continuation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();

            let mut llm_rx = event_loop
                .llm_rx
                .take()
                .expect("test fixture must observe LlmRequest before any worker starts");

            let plan_path = std::env::temp_dir().join(format!(
                "finch_363_plan_wedge_{}.md",
                uuid::Uuid::new_v4()
            ));
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::Planning {
                task: "explore".to_string(),
                plan_path: plan_path.clone(),
                created_at: chrono::Utc::now(),
            };

            let query_id = event_loop.query_states.create_query(Vec::new()).await;
            *event_loop.active_query_id.write().await = Some(query_id);

            let present_plan_id = "toolu_363_present_plan".to_string();
            let present_plan_use = crate::tools::ToolUse {
                id: present_plan_id.clone(),
                name: "present_plan".to_string(),
                input: serde_json::json!({"plan": "1. Do the thing\n2. Verify the thing"}),
            };
            let round_token = event_loop
                .conversation
                .write()
                .await
                .stage_assistant(
                    query_id,
                    crate::providers::Message {
                        role: "assistant".to_string(),
                        content: vec![crate::providers::ContentBlock::ToolUse {
                            id: present_plan_id.clone(),
                            name: "present_plan".to_string(),
                            input: present_plan_use.input.clone(),
                        }],
                    },
                )
                .expect("stage the provider tool round dispatch_tool_uses consumes");
            assert!(
                event_loop
                    .query_states
                    .begin_tool_execution(query_id, 1)
                    .await,
                "production stages ExecutingTools before dispatch_tool_uses runs"
            );

            let work_unit = output.start_work_unit("present-plan wedge regression");

            // Clone every argument `dispatch_tool_uses` needs out of Arcs so the
            // pinned call below borrows nothing from `event_loop` -- keeping
            // `event_loop.handle_event` (a `&mut self` call) legal from inside
            // the pump loop below while the dispatch future is still live.
            let mode_arc = Arc::clone(&event_loop.mode);
            let tool_call_history_arc = Arc::clone(&event_loop.tool_call_history);
            let event_tx_clone = event_loop.event_tx.clone();
            let active_tool_uses_arc = Arc::clone(&event_loop.active_tool_uses);
            let tui_renderer_arc = Arc::clone(&event_loop.tui_renderer);
            let output_manager_arc = Arc::clone(&event_loop.output_manager);
            let query_states_arc = Arc::clone(&event_loop.query_states);
            let tool_coordinator_clone = event_loop.tool_coordinator.clone();
            let memory_system_clone = event_loop.memory_system.clone();
            let session_label_clone = event_loop.session_label.clone();
            let cwd_clone = event_loop.cwd.clone();
            let status_bar_arc = Arc::clone(&event_loop.status_bar);

            let dispatch = crate::cli::repl_event::query_processor::dispatch_tool_uses(
                vec![present_plan_use],
                query_id,
                round_token,
                &work_unit,
                &mode_arc,
                &tool_call_history_arc,
                &event_tx_clone,
                &active_tool_uses_arc,
                &tui_renderer_arc,
                &output_manager_arc,
                &query_states_arc,
                &tool_coordinator_clone,
                &memory_system_clone,
                finch_memory::Recall::none(),
                &session_label_clone,
                &cwd_clone,
                &status_bar_arc,
                4,
            );
            tokio::pin!(dispatch);

            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut dispatch_done = false;
            let mut tool_result_handled = false;
            while !(dispatch_done && tool_result_handled) {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                tokio::select! {
                    _ = &mut dispatch, if !dispatch_done => {
                        dispatch_done = true;
                    }
                    event = tokio::time::timeout(remaining, event_loop.event_rx.recv()) => {
                        let event = event.unwrap_or_else(|_| {
                            panic!(
                                "invariant: the real present_plan dispatch/approval/finalize \
                                 pipeline must terminalize within {remaining:?} instead of \
                                 hanging -- dispatch_done={dispatch_done} \
                                 tool_result_handled={tool_result_handled}. A hang here is \
                                 #363's reported wedge: the ReplMode read guard in \
                                 dispatch_tool_uses held across handle_present_plan's \
                                 mode.write() (#26)."
                            )
                        }).expect("event channel must stay open while dispatch is in flight");
                        let is_present_plan_result = matches!(
                            &event,
                            ReplEvent::ToolResult { tool_id, .. } if tool_id == &present_plan_id
                        );
                        if let ReplEvent::ShowDialog { response_tx, .. } = event {
                            response_tx
                                .send(crate::cli::tui::DialogResult::Selected(0))
                                .expect("present_plan dialog receiver must still be waiting");
                        } else {
                            if is_present_plan_result {
                                tool_result_handled = true;
                            }
                            // Drive the real EventLoop dispatch, exactly as
                            // production's run() loop does -- ToolResult here
                            // reaches handle_tool_result -> finalize_tool_execution,
                            // the fast path #363 suspected of bypassing continuation.
                            let _ = event_loop.handle_event(event).await;
                        }
                    }
                }
            }

            assert!(
                matches!(
                    &*event_loop.mode.read().await,
                    crate::cli::repl::ReplMode::Executing { .. }
                ),
                "approving present_plan through the real dispatch path must land mode in Executing"
            );
            assert_eq!(
                *event_loop.active_query_id.read().await,
                Some(query_id),
                "the plan continuation must reuse the in-flight query, not wedge or orphan it"
            );

            let continuation = tokio::time::timeout(std::time::Duration::from_secs(3), llm_rx.recv())
                .await
                .expect("plan approval must send exactly one execution continuation")
                .expect("the LLM request channel must stay open");
            let LlmRequest::Query { id, text, admission, .. } = continuation;
            assert_eq!(id, query_id, "the continuation must carry the same query id");
            assert_eq!(text, "", "the continuation is a tool-round follow-up, not fresh user text");
            assert!(
                admission.is_none(),
                "the plan-approval fast path sends its continuation ungated (finalize_tool_execution); \
                 seeing `admission: Some(_)` here would mean it started going through \
                 commit_tool_round_and_continue instead"
            );

            // A user keeps typing while the approved plan's continuation is
            // still in flight -- exactly what the maintainer reported doing
            // right before the session went dead.
            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: "run the tests too".to_string(),
                })
                .await
                .expect("typing during the plan continuation must not itself hang or error");
            assert_eq!(
                event_loop.pending_queries.len(),
                1,
                "a turn typed while the plan continuation is active must queue, not stall silently; \
                 queued={:?}",
                event_loop.pending_queries
            );
            assert_eq!(
                *event_loop.active_query_id.read().await,
                Some(query_id),
                "queuing a turn must not disturb the still-active plan continuation"
            );

            // The continuation terminalizes with a plain-text response -- the
            // same two-step sequence `process_query_with_tools`'s text-only
            // path performs before emitting `StreamingComplete`.
            let final_response = "Implemented the plan.".to_string();
            let published = event_loop
                .query_states
                .try_publish_completion_content(
                    query_id,
                    final_response.clone(),
                    vec![crate::providers::ContentBlock::Text {
                        text: final_response.clone(),
                    }],
                    &event_loop.conversation,
                )
                .await;
            assert!(
                published,
                "the continuation's terminal completion must publish; the query must not have \
                 gone terminal or cancelled underneath it"
            );
            event_loop
                .handle_event(ReplEvent::StreamingComplete {
                    query_id,
                    full_response: final_response,
                })
                .await
                .expect("the continuation's StreamingComplete must dispatch cleanly");

            assert!(
                event_loop.pending_queries.is_empty(),
                "the plan continuation's terminal completion must drain the queued turn, not \
                 strand it behind a dead query id; queued={:?}",
                event_loop.pending_queries
            );
            let next_id = event_loop
                .active_query_id
                .read()
                .await
                .expect("draining the queued turn must start it as a new active query -- \
                         this is exactly #363's 'subsequent turns receive no GPT response'");
            assert_ne!(
                next_id, query_id,
                "the drained turn must be its own new query, not a reuse of the finished plan query"
            );

            let next = tokio::time::timeout(std::time::Duration::from_secs(3), llm_rx.recv())
                .await
                .expect("the drained turn must send its own LlmRequest::Query")
                .expect("the LLM request channel must stay open for the drained turn");
            let LlmRequest::Query { id: next_req_id, text: next_text, .. } = next;
            assert_eq!(next_req_id, next_id);
            assert_eq!(
                next_text, "run the tests too",
                "the turn queued during plan execution must actually run once the plan's \
                 continuation completes"
            );

            let _ = std::fs::remove_file(&plan_path);
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
                [] as [&str; 0],
                "QueryFailed must drain the queue and restore to composer; queued={:?}",
                event_loop.pending_queries
            );

            let restored = event_loop.tui_renderer.lock().await.get_input_draft();
            assert_eq!(
                restored, "steer now\nand also this",
                "the queued turns must be restored into the TUI input composer"
            );

            let after_fail = event_loop.conversation.read().await.get_messages();
            assert!(
                !user_text_messages(&after_fail)
                    .iter()
                    .any(|text| text == "steer now"),
                "the first restored turn must not start as its own query; messages={after_fail:?}"
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

type ObservedLlmQueryWithEcho = (Uuid, String, Vec<crate::providers::Message>, Option<String>);

/// Like `observe_llm_queries`, but also captures `pending_echo` -- the value
/// `process_query_with_tools` uses to defer a query's scrollback echo until
/// after its own memory-recall notice commits
/// (`test_memory_notice_commits_before_user_echo_in_scrollback`). Needed to
/// prove the queued-turn drain now supplies that same deferral contract
/// instead of hardcoding `None` after an already-written eager echo.
fn observe_llm_queries_with_echo(
    event_loop: &mut EventLoop,
) -> tokio::sync::mpsc::UnboundedReceiver<ObservedLlmQueryWithEcho> {
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
            pending_echo,
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
            let snapshot = conversation.read().await.get_messages();
            if let Some(spawned) = spawned {
                let _ = spawned.send(());
            }
            if let Some(publication) = publication {
                if publication.await.is_err() {
                    continue;
                }
            }
            let _ = observed_tx.send((id, text, snapshot, pending_echo));
        }
    });
    observed_rx
}

/// Regression for the queued-turn echo/recall-notice ordering bug (companion
/// to #1194, which fixed only the fresh-query path). A user turn submitted
/// while another turn is still actively generating (no tool round in
/// flight, so `finalize_tool_execution`'s merge does not apply) takes the
/// `pending_queries` branch in `execute_query_inner`. Before this fix, that
/// branch wrote the scrollback echo synchronously at queue time, then
/// re-dispatched the same text as a brand-new query -- with its own memory
/// recall -- only once the active turn's `StreamingComplete` fired later.
/// The already-committed echo could never be reordered behind that new
/// turn's own recall notice, reproducing #1194's exact defect through the
/// queue instead of the fresh-query path #1194 patched.
///
/// This drives the real queueing/drain path through `EventLoop::handle_event`
/// end to end (not a `process_query_with_tools` helper) and checks the two
/// places the fix touches: (1) queuing a turn behind an active generation
/// must not commit its echo to `OutputManager` early, and (2) once
/// `StreamingComplete` promotes the queued turn to a new query, that
/// query's `LlmRequest::Query::pending_echo` must carry the queued text --
/// the same deferred-echo contract
/// `test_memory_notice_commits_before_user_echo_in_scrollback` proves is
/// recall-notice-safe in `process_query_with_tools` -- instead of the `None`
/// this path hardcoded before the fix.
#[tokio::test]
async fn test_queued_turn_defers_echo_like_fresh_query_before_streaming_complete() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let mut observed_rx = observe_llm_queries_with_echo(&mut event_loop);

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
                    input: "queued while generating".to_string(),
                })
                .await
                .expect("a second turn during Processing must queue, not double-generate");

            let queued_before_drain = output.get_messages();
            assert!(
                !queued_before_drain
                    .iter()
                    .any(|m| m.content() == "queued while generating"),
                "queuing a turn behind an active generation must not commit its echo to \
                 scrollback yet -- an eager echo here cannot later be reordered behind \
                 that turn's own memory-recall notice once it is promoted to a new query; \
                 rows={:?}",
                queued_before_drain
                    .iter()
                    .map(|m| m.content())
                    .collect::<Vec<_>>()
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

            let second_id = event_loop
                .active_query_id
                .read()
                .await
                .expect("draining pending on StreamingComplete must start the queued turn");
            assert_ne!(
                second_id, first_id,
                "the queued turn is a new query, not a continuation of the finished one"
            );

            let second =
                tokio::time::timeout(std::time::Duration::from_secs(3), observed_rx.recv())
                    .await
                    .expect("StreamingComplete must start the queued turn as LlmRequest::Query")
                    .expect("the LLM request channel must stay open for the drained turn");
            assert_eq!(second.0, second_id);
            assert_eq!(second.1, "queued while generating");
            assert_eq!(
                second.3,
                Some("queued while generating".to_string()),
                "the queued turn promoted to a new query must carry pending_echo so \
                 process_query_with_tools defers the scrollback echo until after its own \
                 memory-recall notice commits, exactly like a fresh query; observed={second:?}"
            );

            assert!(
                !output
                    .get_messages()
                    .iter()
                    .any(|m| m.content() == "queued while generating"),
                "the event loop must hand the echo text to process_query_with_tools via \
                 pending_echo rather than committing it itself; a row here would mean the \
                 eager write is still happening"
            );
        })
        .await;
}

/// A generator that always claims streaming support and answers through
/// `generate_stream_cancellable`, never `generate()` -- the branch local
/// models (Gemma/Qwen via `DaemonLocalGenerator`, streaming since #1216)
/// take. `generate()` panics so a silent fallback to non-streaming would
/// fail the test loudly instead of passing for the wrong reason.
struct StreamingEchoGenerator;

#[async_trait::async_trait]
impl crate::generators::Generator for StreamingEchoGenerator {
    async fn generate(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<crate::generators::GeneratorResponse> {
        panic!(
            "StreamingEchoGenerator::generate must never be called -- this \
             fixture exists to exercise the real streaming branch end to \
             end through a genuine LlmLoop worker"
        );
    }

    async fn generate_stream(
        &self,
        messages: Vec<crate::providers::Message>,
        tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<
        Option<tokio::sync::mpsc::Receiver<anyhow::Result<crate::generators::StreamChunk>>>,
    > {
        self.generate_stream_cancellable(
            messages,
            tools,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
    }

    async fn generate_stream_cancellable(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
        _cancellation_token: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<
        Option<tokio::sync::mpsc::Receiver<anyhow::Result<crate::generators::StreamChunk>>>,
    > {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tx.send(Ok(crate::generators::StreamChunk::TextDelta(
            "(say \"ack\")".to_string(),
        )))
        .await
        .expect("seed the paced stream with its one chunk");
        drop(tx);
        Ok(Some(rx))
    }

    fn capabilities(&self) -> &crate::generators::GeneratorCapabilities {
        static CAPS: crate::generators::GeneratorCapabilities =
            crate::generators::GeneratorCapabilities {
                supports_streaming: true,
                supports_tools: true,
                supports_conversation: true,
                max_context_messages: None,
            };
        &CAPS
    }

    fn name(&self) -> &str {
        "streaming-echo-generator"
    }
}

/// Content long enough to survive the memory quality classifier's noise
/// filter and specific enough that hashed-n-gram retrieval reliably ranks it
/// for a matching query -- mirrors `query_processor.rs`'s own
/// `substantive_memory`/`memory_system_with_seed_for_test` test fixtures.
async fn seeded_memory_system_for_test(tag: &str) -> Arc<finch_memory::MemorySystem> {
    let temp = tempfile::NamedTempFile::new().expect("create temp memory db");
    let memory = finch_memory::MemorySystem::new(finch_memory::MemoryConfig {
        db_path: temp.path().to_path_buf(),
        ..Default::default()
    })
    .expect("construct test memory system");
    memory
        .insert_conversation(
            "user",
            &format!("Where is the deploy key for the {tag} environment?"),
            None,
            None,
        )
        .await
        .expect("seed recall question");
    memory
        .insert_conversation(
            "assistant",
            &format!(
                "The deploy key for the {tag} environment lives in the Employee \
                 vault under the Finch signing item, not in the repository."
            ),
            None,
            None,
        )
        .await
        .expect("seed recall answer");
    // Leak the tempfile for the test's lifetime: `MemorySystem` keeps the
    // path open and a test-scoped `NamedTempFile` would delete it on drop
    // while the `EventLoop`/`LlmLoop` under test are still using it.
    std::mem::forget(temp);
    Arc::new(memory)
}

/// Production-boundary regression for issue #1248: on a plain, idle,
/// non-queued local-model turn, the memory-recall notice ("N memories
/// retrieved") must still commit to scrollback before the user's own
/// echoed question -- the same ordering guarantee #1194 established for the
/// fresh-query path and #1242 extended to the queued-turn path.
///
/// #1194's own regression drives `process_query_with_tools` directly with
/// hand-built arguments; #1242's drives `EventLoop::handle_event` but
/// intercepts the `LlmRequest` off the channel before any worker consumes
/// it. Neither exercises the real, wired-together
/// `EventLoop::handle_event` -> `llm_tx` channel -> a genuinely spawned
/// `LlmLoop::run()` -> `spawn_query` -> `process_query_with_tools` path --
/// the same seam `Repl::run_event_loop` uses in production. This test
/// closes that gap: `EventLoop::new_local_streaming_memory_test_runner`
/// wires a real `finch_memory::MemorySystem` seeded with a matching
/// exchange and a `StreamingEchoGenerator` that only answers through the
/// streaming branch, `start_llm_worker()` spawns the real worker task (the
/// same call `EventLoop::run` makes in production), and a plain
/// `ReplEvent::UserInput` is submitted exactly as `handle_user_input` does
/// for ordinary typed text -- never `??`, never a queued
/// turn.
#[tokio::test]
async fn test_plain_streaming_local_turn_commits_memory_notice_before_echo() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let query_text = "Where is the deploy key for the staging environment?";
            let memory = seeded_memory_system_for_test("staging").await;
            let mut event_loop = EventLoop::new_local_streaming_memory_test_runner(
                Arc::new(StreamingEchoGenerator),
                memory,
            );
            let output = Arc::clone(&event_loop.output_manager);
            output.disable_stdout();

            // Spawns the real `LlmLoop::run()` worker task, exactly as
            // `EventLoop::run` does in production -- the seam #1194's and
            // #1242's own regressions never drove.
            event_loop.start_llm_worker();

            event_loop
                .handle_event(ReplEvent::UserInput {
                    input: query_text.to_string(),
                })
                .await
                .expect("a plain, idle submission must start a fresh query");

            // The worker task, memory recall, and the streaming generation
            // all run asynchronously now; poll for the turn to finish
            // committing both rows instead of asserting on a fixed delay.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let messages = output.get_messages();
                let has_echo = messages.iter().any(|m| m.content() == query_text);
                let has_notice = messages.iter().any(|m| {
                    m.format(&crate::theme::ColorScheme::default())
                        .contains("retrieved")
                });
                if has_echo && has_notice {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    panic!(
                        "the real LlmLoop worker never committed both the echo and the \
                         memory-recall notice within the timeout; rows={:?}",
                        messages.iter().map(|m| m.content()).collect::<Vec<_>>()
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            let messages = output.get_messages();
            let row_previews: Vec<String> = messages.iter().map(|m| m.content()).collect();
            let colors = crate::theme::ColorScheme::default();
            let notice_idx = messages
                .iter()
                .position(|m| m.format(&colors).contains("retrieved"))
                .unwrap_or_else(|| {
                    panic!("expected a memory-recall notice row; rows={row_previews:?}")
                });
            let echo_idx = messages
                .iter()
                .position(|m| m.content() == query_text)
                .unwrap_or_else(|| {
                    panic!("expected the user's own echoed question row; rows={row_previews:?}")
                });
            assert!(
                notice_idx < echo_idx,
                "the memory-recall notice must commit to scrollback before the user's \
                 own echoed question on a plain, idle, non-queued local-model turn \
                 (issue #1248) -- notice_idx={notice_idx}, echo_idx={echo_idx}, \
                 rows={row_previews:?}"
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

/// Queue two turns behind an in-flight tool round, the fixture every
/// "queued text must end up in exactly one place" test below starts from.
async fn queue_two_turns_behind_tool_round(event_loop: &mut EventLoop) {
    for input in ["first queued", "second queued"] {
        event_loop
            .handle_event(ReplEvent::UserInput {
                input: input.to_string(),
            })
            .await
            .expect("queuing a user turn during ExecutingTools must succeed");
    }
    assert_eq!(
        event_loop
            .pending_queries
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<Vec<_>>(),
        ["first queued", "second queued"],
        "the fixture must hold both turns on pending_queries in FIFO order"
    );
}

/// Text unique to the prompt `/plan` generates from the vocabulary stack.
const GENERATED_PROMPT_NEEDLE: &str = "building a vocabulary";

/// Queue a prompt the user never typed, through the real `/plan` path: with
/// two words on the vocabulary stack it synthesises a prompt and submits it
/// with `echo = false`, which queues behind the in-flight turn.
async fn queue_generated_plan_prompt(event_loop: &mut EventLoop) {
    event_loop
        .stack
        .lock()
        .await
        .extend(["alpha".to_string(), "beta".to_string()]);
    event_loop
        .handle_event(ReplEvent::UserInput {
            input: "/plan".to_string(),
        })
        .await
        .expect("/plan with two stacked words must queue its synthesis prompt");
    let last = event_loop.pending_queries.back();
    assert!(
        matches!(last, Some((text, false, true)) if text.contains(GENERATED_PROMPT_NEEDLE)),
        "the fixture must hold the generated prompt, unechoed, at the back of the queue; last={last:?}"
    );
}

/// The transcript must say in words how many queued messages went back to
/// the input box; the composer changing is not announced on its own.
fn assert_return_announced_once(
    output: &crate::cli::output_manager::OutputManager,
    expected: &str,
) {
    let rows: Vec<String> = output
        .get_messages()
        .iter()
        .map(|message| message.content())
        .collect();
    let announcing = rows
        .iter()
        .filter(|row| row.contains("returned to the input box"))
        .collect::<Vec<_>>();
    assert!(
        announcing.len() == 1 && announcing[0].contains(expected),
        "exactly one transcript row must state {expected:?}; rows={rows:?}"
    );
}

/// How many times `needle` appears across the composer draft, the
/// provider-visible conversation, and the transcript's rows: the three
/// places queued text can end up.
async fn queued_text_locations(
    event_loop: &EventLoop,
    output: &crate::cli::output_manager::OutputManager,
    needle: &str,
) -> (usize, usize, usize) {
    let draft = event_loop.tui_renderer.lock().await.get_input_draft();
    let in_conversation = event_loop
        .conversation
        .read()
        .await
        .get_messages()
        .iter()
        .flat_map(|message| message.content.iter())
        .filter(|block| {
            matches!(block, crate::providers::ContentBlock::Text { text } if text.contains(needle))
        })
        .count();
    let in_transcript = output
        .get_messages()
        .iter()
        .filter(|message| message.content().contains(needle))
        .count();
    (
        draft.matches(needle).count(),
        in_conversation,
        in_transcript,
    )
}

/// The issue's literal scenario (#1587, queued messages lost when a tool
/// fails or times out): a tool that times out still completes its round, so
/// text queued behind it must ride the continuation, once, in order.
#[tokio::test]
async fn test_queued_messages_ride_the_continuation_when_the_tool_times_out() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let mut observed_rx = observe_llm_queries(&mut event_loop);
            let tool_id = "call_times_out";
            let (query_id, round_token) =
                start_executing_tools_query(&mut event_loop, tool_id).await;
            queue_two_turns_behind_tool_round(&mut event_loop).await;

            event_loop
                .handle_event(ReplEvent::ToolResult {
                    query_id,
                    round_token,
                    tool_id: tool_id.to_string(),
                    result: Err(anyhow::anyhow!(
                        "Tool execution timed out after 30 seconds. \
                         Try restarting or check daemon logs for errors."
                    )),
                })
                .await
                .expect("a timed-out tool result must still finalize its round");

            let observed =
                tokio::time::timeout(std::time::Duration::from_secs(3), observed_rx.recv())
                    .await
                    .expect("a timed-out tool round must still send its continuation (hung waiting for LlmRequest::Query)")
                    .expect("the LLM request channel must stay open for the continuation");
            assert_eq!(
                observed.0, query_id,
                "the continuation must reuse the in-flight query; observed={observed:?}"
            );
            let last = observed
                .2
                .last()
                .expect("the continuation must carry the tool-result turn");
            assert!(
                matches!(
                    last.content.as_slice(),
                    [
                        crate::providers::ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error: Some(true)
                        },
                        crate::providers::ContentBlock::Text { text: first },
                        crate::providers::ContentBlock::Text { text: second },
                    ] if tool_use_id == tool_id
                        && content.contains("timed out")
                        && first == "first queued"
                        && second == "second queued"
                ),
                "the provider must see the timeout result followed by both queued turns in order; last={last:?}"
            );
            assert!(
                event_loop.pending_queries.is_empty(),
                "the tool-round boundary must consume the queue; queued={:?}",
                event_loop.pending_queries
            );
            for needle in ["first queued", "second queued"] {
                let found = queued_text_locations(&event_loop, &output, needle).await;
                assert_eq!(
                    found,
                    (0, 1, 1),
                    "after a timed-out tool, {needle:?} must be in the conversation once and echoed once, and not in the draft; (draft, conversation, transcript)={found:?}"
                );
            }
        })
        .await;
}

/// A provider error in the middle of a tool round ends the turn with no
/// later boundary to consume the queue. Every queued turn must come back to
/// the composer once, in order, ahead of the text being typed, and none may
/// run or be echoed.
#[tokio::test]
async fn test_provider_failure_mid_round_returns_queued_messages_ahead_of_the_draft() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let mut observed_rx = observe_llm_queries(&mut event_loop);
            let (query_id, _round_token) =
                start_executing_tools_query(&mut event_loop, "call_provider_fails").await;
            queue_two_turns_behind_tool_round(&mut event_loop).await;
            queue_generated_plan_prompt(&mut event_loop).await;
            event_loop
                .tui_renderer
                .lock()
                .await
                .restore_input_draft("half typed");

            event_loop
                .handle_event(ReplEvent::QueryFailed {
                    query_id,
                    error: "provider returned 529 overloaded".to_string(),
                    generator_name: None,
                })
                .await
                .expect("QueryFailed must dispatch");

            let draft = event_loop.tui_renderer.lock().await.get_input_draft();
            assert_eq!(
                draft, "first queued\nsecond queued\nhalf typed",
                "a failed turn must return every queued message to the composer in order, keeping the text being typed after them; queued={:?}",
                event_loop.pending_queries
            );
            assert!(
                event_loop.pending_queries.is_empty(),
                "returned turns must leave the queue so they cannot also run later; queued={:?}",
                event_loop.pending_queries
            );
            assert_eq!(
                *event_loop.active_query_id.read().await,
                None,
                "the failed turn must release the active-query slot"
            );
            // A late completion for the failed query must not start anything.
            event_loop
                .handle_event(ReplEvent::StreamingComplete {
                    query_id,
                    full_response: "late prose".to_string(),
                })
                .await
                .expect("a late StreamingComplete for the failed query must be discarded");
            assert!(
                observed_rx.try_recv().is_err(),
                "a failed turn must not dispatch a queued turn to the provider"
            );
            for needle in ["first queued", "second queued"] {
                let found = queued_text_locations(&event_loop, &output, needle).await;
                assert_eq!(
                    found,
                    (1, 0, 0),
                    "after a provider failure, {needle:?} must be in the draft exactly once and nowhere else; (draft, conversation, transcript)={found:?}"
                );
            }
            let generated =
                queued_text_locations(&event_loop, &output, GENERATED_PROMPT_NEEDLE).await;
            assert_eq!(
                generated,
                (0, 0, 0),
                "a prompt the user never typed must not be put in the composer, sent, or echoed; (draft, conversation, transcript)={generated:?}"
            );
            assert_return_announced_once(&output, "2 queued messages returned to the input box");
        })
        .await;
}

/// Escape during a tool round must not throw away what the user queued
/// behind it. The queued turns must not run (#463, queued turn must not
/// execute out of order after cancel); they return to the composer once, in
/// order, ahead of the text being typed.
#[tokio::test]
async fn test_cancel_returns_queued_messages_ahead_of_the_draft() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let mut observed_rx = observe_llm_queries(&mut event_loop);
            let (query_id, _round_token) =
                start_executing_tools_query(&mut event_loop, "call_cancelled").await;
            queue_two_turns_behind_tool_round(&mut event_loop).await;
            queue_generated_plan_prompt(&mut event_loop).await;
            event_loop
                .tui_renderer
                .lock()
                .await
                .restore_input_draft("half typed");

            event_loop
                .handle_event(ReplEvent::CancelQuery)
                .await
                .expect("CancelQuery must dispatch");

            let draft = event_loop.tui_renderer.lock().await.get_input_draft();
            assert_eq!(
                draft, "first queued\nsecond queued\nhalf typed",
                "Escape must return every queued message to the composer in order, keeping the text being typed after them; queued={:?}",
                event_loop.pending_queries
            );
            assert!(
                event_loop.pending_queries.is_empty(),
                "returned turns must leave the queue so they cannot re-fire after a later turn; queued={:?}",
                event_loop.pending_queries
            );
            // The cancelled provider may still report either terminal event.
            event_loop
                .handle_event(ReplEvent::QueryFailed {
                    query_id,
                    error: "request aborted".to_string(),
                    generator_name: None,
                })
                .await
                .expect("a late QueryFailed for the cancelled query must be discarded");
            event_loop
                .handle_event(ReplEvent::StreamingComplete {
                    query_id,
                    full_response: "late prose".to_string(),
                })
                .await
                .expect("a late StreamingComplete for the cancelled query must be discarded");
            assert!(
                observed_rx.try_recv().is_err(),
                "a cancelled turn must not dispatch a queued turn to the provider"
            );
            for needle in ["first queued", "second queued", "half typed"] {
                let found = queued_text_locations(&event_loop, &output, needle).await;
                assert_eq!(
                    found,
                    (1, 0, 0),
                    "after Escape, {needle:?} must be in the draft exactly once and nowhere else, including after the cancelled provider's late terminal events; (draft, conversation, transcript)={found:?}"
                );
            }
            let generated =
                queued_text_locations(&event_loop, &output, GENERATED_PROMPT_NEEDLE).await;
            assert_eq!(
                generated,
                (0, 0, 0),
                "a prompt the user never typed must not be put in the composer, sent, or echoed; (draft, conversation, transcript)={generated:?}"
            );
            assert_return_announced_once(&output, "2 queued messages returned to the input box");
        })
        .await;
}

/// The ordinary Escape case: the composer is empty when the cancel is
/// handled (Escape only requests a cancel from an empty draft), so the
/// returned messages are the whole draft.
#[tokio::test]
async fn test_cancel_with_empty_draft_returns_queued_messages_in_order() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, output) = lifecycle_test_event_loop();
            output.disable_stdout();
            let mut observed_rx = observe_llm_queries(&mut event_loop);
            let (_query_id, _round_token) =
                start_executing_tools_query(&mut event_loop, "call_cancelled_empty").await;
            queue_two_turns_behind_tool_round(&mut event_loop).await;

            event_loop
                .handle_event(ReplEvent::CancelQuery)
                .await
                .expect("CancelQuery must dispatch");

            let draft = event_loop.tui_renderer.lock().await.get_input_draft();
            assert_eq!(
                draft, "first queued\nsecond queued",
                "Escape on an empty composer must leave exactly the queued messages in it, in order; queued={:?}",
                event_loop.pending_queries
            );
            assert!(
                event_loop.pending_queries.is_empty() && observed_rx.try_recv().is_err(),
                "returned turns must neither stay queued nor be dispatched; queued={:?}",
                event_loop.pending_queries
            );
            assert_return_announced_once(&output, "2 queued messages returned to the input box");
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
                        grant_ceiling: crate::vm::TypedRuntime::intrinsic_grants(),
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

/// Cancelling a query (Esc — Ctrl+C no longer cancels, it copies the
/// transcript selection instead) must keep AutoAccept so dogfood does not
/// fall back to per-tool prompts mid-session.
#[tokio::test]
async fn test_cancel_query_during_query_preserves_auto_accept() {
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
                "cancelling a query must keep AutoAccept; mode={mode:?}"
            );
        })
        .await;
}

/// Idle cancel (Esc) in AutoAccept exits Finch, like Normal. It must not be
/// treated as a plan overlay that silently drops back to confirmation mode.
#[tokio::test]
async fn test_idle_cancel_in_auto_accept_exits_finch() {
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
                "idle cancel must not drop AutoAccept into Normal; mode={mode:?}"
            );
            let mut found_shutdown = false;
            while let Ok(event) = event_loop.event_rx.try_recv() {
                if matches!(event, super::ReplEvent::Shutdown) {
                    found_shutdown = true;
                }
            }
            assert!(
                found_shutdown,
                "idle cancel in AutoAccept must exit Finch by sending Shutdown"
            );
        })
        .await;
}

/// #1311 production-boundary regression: an idle, empty-composer Escape
/// press on a SINGLE press used to send `CancelQuery` straight through,
/// which the idle branch exits Finch on entirely, with zero warning ever
/// shown — worse than the Ctrl+C case #1301 fixed, since that at least took
/// two presses. This exercises the real chain the fix added:
/// `async_input::handle_composer_shortcuts` setting `pending_escape_cancel`
/// on an idle Escape (asserted directly in `finch-tui`'s own
/// `test_keyboard_shortcut_table_matches_the_real_dispatch_paths`), and here
/// the application side that flag feeds: `EventLoop::handle_escape_cancel_request`
/// arms on the first idle press instead of exiting, `EventLoop::sync_escape_exit_hint`
/// puts "Press Esc again to exit Finch" on the status line (reaching the real
/// `StatusBar::status_without_session`), and only a confirming second idle
/// press within `ESCAPE_IDLE_EXIT_WINDOW` actually dispatches `CancelQuery`.
#[tokio::test]
async fn test_idle_escape_arm_shows_exit_warning_then_confirms_on_second_press() {
    use crate::cli::tui::TuiStatusPort;

    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::Normal;

            assert!(
                !event_loop
                    .status_bar
                    .status_without_session()
                    .contains("Esc again"),
                "no warning must be showing before any Escape press"
            );

            // First idle Escape press: simulates the render tick taking
            // `pending_escape_cancel` from the renderer and routing it here.
            event_loop.handle_escape_cancel_request().await;
            event_loop.sync_escape_exit_hint().await;

            let status = event_loop.status_bar.status_without_session();
            assert!(
                status.contains("Press Esc again to exit Finch"),
                "the first idle Escape press must arm and warn, never exit \
                 silently on a single press; status={status:?}"
            );
            let mut fired_after_first_press = false;
            while let Ok(event) = event_loop.event_rx.try_recv() {
                if matches!(event, super::ReplEvent::CancelQuery) {
                    fired_after_first_press = true;
                }
            }
            assert!(
                !fired_after_first_press,
                "the first idle Escape press must not dispatch CancelQuery; \
                 doing so is exactly the #1311 silent single-press exit"
            );

            // Confirming second idle Escape press within the window: now it
            // actually requests the cancel that the idle CancelQuery branch
            // turns into exiting Finch, and the warning clears.
            event_loop.handle_escape_cancel_request().await;
            event_loop.sync_escape_exit_hint().await;

            let status = event_loop.status_bar.status_without_session();
            assert!(
                !status.contains("Esc again"),
                "the warning must clear once the confirming press has fired; \
                 status={status:?}"
            );
            let mut fired_after_second_press = false;
            while let Ok(event) = event_loop.event_rx.try_recv() {
                if matches!(event, super::ReplEvent::CancelQuery) {
                    fired_after_second_press = true;
                }
            }
            assert!(
                fired_after_second_press,
                "the confirming second idle Escape press must dispatch \
                 CancelQuery so the user can still actually exit via Escape"
            );
        })
        .await;
}

/// Escape's cancel of an ACTIVE query must stay a single, immediate press —
/// explicitly called out as unchanged by #1311. Unlike Ctrl+C (which arms
/// even with a query running), Escape must not gain a confirm step here:
/// only the branch that would actually exit Finch entirely gets one.
#[tokio::test]
async fn test_escape_with_active_query_cancels_immediately_without_exit_warning() {
    use crate::cli::tui::TuiStatusPort;

    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::Normal;
            let query_id = event_loop.query_states.create_query(Vec::new()).await;
            *event_loop.active_query_id.write().await = Some(query_id);

            event_loop.handle_escape_cancel_request().await;
            event_loop.sync_escape_exit_hint().await;

            let status = event_loop.status_bar.status_without_session();
            assert!(
                !status.contains("exit Finch"),
                "Escape cancels the active query, not Finch, on this single \
                 press — the exit warning must not show; status={status:?}"
            );

            let mut found_cancel = false;
            while let Ok(event) = event_loop.event_rx.try_recv() {
                if matches!(event, super::ReplEvent::CancelQuery) {
                    found_cancel = true;
                }
            }
            assert!(
                found_cancel,
                "a single idle-composer Escape press must still cancel an \
                 active query immediately — #1311 must not regress this \
                 into requiring a second press"
            );
            assert!(
                event_loop.escape_idle_exit_armed_at.is_none(),
                "cancelling an active query must not arm the idle-exit \
                 confirm state — there is nothing to confirm"
            );
        })
        .await;
}

/// A plan/executing overlay's Escape exits the overlay, not Finch
/// (`ReplMode::is_plan_overlay`), so it keeps Escape's single-press
/// character too — same reasoning as the active-query case above.
#[tokio::test]
async fn test_escape_in_plan_overlay_cancels_immediately_without_exit_warning() {
    use crate::cli::tui::TuiStatusPort;

    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::Planning {
                task: "investigate #1311".to_string(),
                plan_path: std::path::PathBuf::from("/tmp/plan.md"),
                created_at: chrono::Utc::now(),
            };

            event_loop.handle_escape_cancel_request().await;
            event_loop.sync_escape_exit_hint().await;

            let status = event_loop.status_bar.status_without_session();
            assert!(
                !status.contains("exit Finch"),
                "Escape exits the plan overlay, not Finch, on this single \
                 press; status={status:?}"
            );

            let mut found_cancel = false;
            while let Ok(event) = event_loop.event_rx.try_recv() {
                if matches!(event, super::ReplEvent::CancelQuery) {
                    found_cancel = true;
                }
            }
            assert!(
                found_cancel,
                "a single idle-composer Escape press in a plan overlay must \
                 still dispatch CancelQuery immediately, unchanged"
            );
        })
        .await;
}

/// Mirrors `test_composer_ctrl_c_after_window_elapses_does_not_cancel_but_rearms`
/// (finch-tui): an idle Escape press after `ESCAPE_IDLE_EXIT_WINDOW` has
/// elapsed must not count as the confirming press of a stale arm — it
/// starts a fresh arm instead of exiting on what looks like a lone press
/// spread across two unrelated moments.
#[tokio::test]
async fn test_escape_idle_exit_arm_expires_after_window() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::Normal;

            event_loop.handle_escape_cancel_request().await;
            assert!(
                event_loop.escape_idle_exit_armed_at.is_some(),
                "the first idle Escape press must record an arm timestamp"
            );

            // Backdate the arm past the window, as if the second press
            // arrived too late.
            let armed_at = event_loop.escape_idle_exit_armed_at.expect("armed above");
            event_loop.escape_idle_exit_armed_at = Some(
                armed_at - super::ESCAPE_IDLE_EXIT_WINDOW - std::time::Duration::from_millis(1),
            );

            event_loop.handle_escape_cancel_request().await;

            let mut found_cancel = false;
            while let Ok(event) = event_loop.event_rx.try_recv() {
                if matches!(event, super::ReplEvent::CancelQuery) {
                    found_cancel = true;
                }
            }
            assert!(
                !found_cancel,
                "a press after the window elapsed must not count as the \
                 confirming press and must not dispatch CancelQuery"
            );
            assert!(
                event_loop.escape_idle_exit_armed_at.is_some(),
                "the lapsed press must start a fresh arm rather than \
                 leaving no arm at all"
            );
        })
        .await;
}

/// #1311 code-review regression: an idle-exit arm left over from an earlier
/// press must not survive an intervening active-query cancel. Before this
/// fix, `handle_escape_cancel_request`'s immediate-cancel branch (active
/// query or plan overlay) never touched `escape_idle_exit_armed_at`, so a
/// LATER, logically unrelated idle Escape press within the original arm's
/// window would read the stale arm as its own confirming second press and
/// exit Finch — for a press that had no warning shown for it specifically.
#[tokio::test]
async fn test_escape_active_query_cancel_clears_a_stale_idle_arm() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir) = auto_accept_event_loop();
            *event_loop.mode.write().await = crate::cli::repl::ReplMode::Normal;

            // First idle Escape press: arms the idle-exit confirm state.
            event_loop.handle_escape_cancel_request().await;
            assert!(
                event_loop.escape_idle_exit_armed_at.is_some(),
                "the first idle Escape press must arm"
            );

            // A query starts and an Escape press cancels it — unrelated to
            // the idle-exit gesture above, but reachable within the same
            // arm window in real usage.
            let query_id = event_loop.query_states.create_query(Vec::new()).await;
            *event_loop.active_query_id.write().await = Some(query_id);
            event_loop.handle_escape_cancel_request().await;
            assert!(
                event_loop.escape_idle_exit_armed_at.is_none(),
                "cancelling an active query must clear any stale idle-exit \
                 arm left over from an earlier, unrelated idle press"
            );
            *event_loop.active_query_id.write().await = None;
            while event_loop.event_rx.try_recv().is_ok() {}

            // A later idle Escape press, still inside the ORIGINAL arm's
            // window, must be treated as a fresh first press (arm again, do
            // not exit) rather than confirming the stale arm.
            event_loop.handle_escape_cancel_request().await;
            let mut found_cancel = false;
            while let Ok(event) = event_loop.event_rx.try_recv() {
                if matches!(event, super::ReplEvent::CancelQuery) {
                    found_cancel = true;
                }
            }
            assert!(
                !found_cancel,
                "a later idle Escape press must not silently exit Finch by \
                 confirming a stale arm left over from an unrelated \
                 intervening query cancel"
            );
            assert!(
                event_loop.escape_idle_exit_armed_at.is_some(),
                "the later idle press must (re)arm instead of exiting"
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
    let haystack = format!(
        "{} {:?} {:?}",
        projected.label, projected.body, projected.children
    );
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

/// Reproduces #423's reported reattach through the real startup path:
/// `EventLoop::run` feeds `register_home_brain`'s result straight into
/// `apply_home_runner_startup`, so a stale lease from a closed console
/// surfaces here first, exactly as it does live. Before the fix this printed
/// the raw daemon exception text under a "runner unavailable" header — a
/// failure-shaped line for a condition the bounded reconnect (already
/// scheduled a few lines below in production) resolves on its own.
#[tokio::test]
async fn test_stale_lease_reattach_reports_calm_transition_not_raw_failure() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut event_loop = runner_recovery_test_event_loop();
            let attach =
                event_loop.apply_home_runner_startup(Ok(Some(super::HomeRunnerRegistration {
                    target: crate::cli::repl_event::events::RunnerReconnectTarget {
                        brain: "pale-glen-de0e39".into(),
                        environment: crate::brain::BrainEnvironment {
                            machine: "box.local".into(),
                            workspace: std::path::PathBuf::from("/tmp/ws"),
                            generation: 1,
                        },
                        lease_id: None,
                    },
                    registration: Err(
                        "Failed: remote exception: Brain already has a live runner lease".into(),
                    ),
                })));
            assert!(
                attach,
                "a stale lease pending its own reconnect must still allow this console to \
                 attach as a driver; attach={attach}"
            );

            let messages = runner_recovery_messages(&event_loop);
            assert!(
                messages
                    .iter()
                    .all(|message| !message.contains("remote exception")
                        && !message.contains("Failed:")),
                "a stale lease is the expected reattach case, not a raw remote-exception \
                 failure line; messages={messages:?}"
            );
            assert!(
                messages.iter().any(|message| message.contains("pale-glen-de0e39")
                    && message.to_lowercase().contains("transferring")),
                "the stale lease must be reported once, naming the brain and the transition \
                 in progress, in place of the three old failure-shaped lines; messages={messages:?}"
            );

            let header = event_loop
                .status_bar
                .get_line(&crate::cli::status_bar::StatusLineType::SessionLabel)
                .expect("startup must project a session header");
            assert!(
                header.contains("transferring"),
                "the status strip must agree with the transcript's calm transition wording, \
                 not say something else; header={header}"
            );
            assert!(
                !header.contains("unavailable") && !header.contains("online"),
                "the status strip must not call a recoverable transition unavailable, nor \
                 falsely claim the runner is already online while it is still transferring; \
                 header={header}"
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
            || recovery.contains("runner unavailable")
            || recovery.contains("workspace mismatch"),
        "header/status must still show a compact runner/home recovery hint; header={header:?} status={status:?} haystack={haystack:?}"
    );
}

/// The maintainer-corrected scope for the raw-dump transcript defect (#907):
/// lease/attach/handoff events BELONG in the transcript on the live path, but
/// they must arrive as structured rows — the icon-prefixed info kind every
/// other static row uses — one row per event, ids bounded, no blank-line
/// chrome. Run-status events keep flowing into the run-group projection and
/// never become notice rows.
#[tokio::test]
async fn test_live_lease_attach_handoff_events_render_structured_info_rows() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::brain::{
                AttachmentId, AttachmentRole, BrainEventKind, BrainRunKind, BrainRunStatus,
                BrainRunnerHandoff, BrainRunnerLease, ConnectionId, RunId, RunnerHandoffId,
                RunnerLeaseId,
            };

            let mut event_loop = runner_recovery_test_event_loop();
            let lease_id = RunnerLeaseId(uuid::Uuid::new_v4());
            let handoff_id = RunnerHandoffId(uuid::Uuid::new_v4());
            let attachment_id = AttachmentId(uuid::Uuid::new_v4());
            let run_id = RunId(uuid::Uuid::new_v4());
            let lease = BrainRunnerLease {
                lease_id,
                subject: "shammah@box.local/frontend-cc388712".into(),
                environment_generation: 1,
                acquired_ms: 0,
                expires_ms: 0,
            };
            let handoff = BrainRunnerHandoff {
                handoff_id,
                from_lease_id: lease_id,
                requested_by: "peer@box.local/observer-1".into(),
                target_subject: event_loop.runner_subject.clone(),
                environment_generation: 1,
                requested_ms: 0,
                expires_ms: 0,
            };
            let events = vec![
                brain_event(
                    1,
                    "daemon",
                    BrainEventKind::RunnerLeaseAcquired {
                        lease: lease.clone(),
                    },
                ),
                brain_event(
                    2,
                    "daemon",
                    BrainEventKind::RunnerLeaseReleased { lease_id },
                ),
                brain_event(
                    3,
                    "daemon",
                    BrainEventKind::RunnerHandoffRequested { handoff },
                ),
                brain_event(
                    4,
                    "daemon",
                    BrainEventKind::RunnerHandoffCompleted {
                        handoff_id,
                        lease: lease.clone(),
                    },
                ),
                brain_event(
                    5,
                    "daemon",
                    BrainEventKind::RunnerHandoffCancelled { handoff_id },
                ),
                brain_event(
                    6,
                    "daemon",
                    BrainEventKind::ClientAttached {
                        attachment_id,
                        connection_id: ConnectionId::default(),
                        subject: "alice@box.local/observer-2".into(),
                        role: AttachmentRole::Consultant,
                    },
                ),
                brain_event(
                    7,
                    "daemon",
                    BrainEventKind::ClientDetached {
                        attachment_id,
                        connection_id: ConnectionId::default(),
                    },
                ),
            ];
            for event in &events {
                event_loop
                    .render_remote_brain_message(crate::brain::BrainWireMessage::Event {
                        event: event.clone(),
                    })
                    .await
                    .expect("live Brain event must dispatch through the render path");
            }

            let colors = crate::theme::ColorScheme::default();
            let messages = event_loop.output_manager.get_messages();
            let rendered = messages
                .iter()
                .map(|message| {
                    format!(
                        "kind={:?} content={:?} format={:?}",
                        message.component_view().map(|view| match view {
                            finch_ui_model::ComponentView::StaticText(text) => text.kind,
                            other => unreachable!("unexpected view {other:?}"),
                        }),
                        message.content(),
                        message.format(&colors)
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                messages.len(),
                events.len(),
                "INVARIANT: every live lease/attach/handoff event renders exactly one \
                 transcript row with no stray blank-line chrome; rows={rendered:#?}"
            );

            let full_handoff_id = handoff_id.0.to_string();
            let full_attachment_id = attachment_id.0.to_string();
            let expected_payloads = [
                "shammah@box.local/frontend-cc388712 is the active environment runner",
                "environment runner disconnected",
                "peer@box.local/observer-1 requested runner handoff to",
                "runner handoff completed to shammah@box.local/frontend-cc388712",
                "runner handoff cancelled",
                "alice@box.local/observer-2 attached as consultant",
                "attachment disconnected",
            ];
            for (message, expected) in messages.iter().zip(expected_payloads) {
                let view = message.component_view().unwrap_or_else(|| {
                    panic!("notice must carry a component view; rows={rendered:#?}")
                });
                let finch_ui_model::ComponentView::StaticText(text) = view else {
                    panic!("notice must be a static text row; rows={rendered:#?}")
                };
                assert!(
                    matches!(text.kind, finch_ui_model::StaticTextKind::Info),
                    "INVARIANT: a lease/attach/handoff row must render as the structured \
                     icon-prefixed info kind, not an unstructured plain dump; kind={:?} \
                     rows={rendered:#?}",
                    text.kind
                );
                let formatted = message.format(&colors);
                assert!(
                    formatted.contains('ℹ') && formatted.contains(expected),
                    "INVARIANT: the info row must carry the info glyph and its payload; \
                     expected={expected:?} formatted={formatted:?} rows={rendered:#?}"
                );
            }
            let handoff_row = &messages[2];
            assert!(
                handoff_row.content().contains(&full_handoff_id[..8])
                    && handoff_row.content().contains("addressed to this frontend"),
                "the handoff row addressed to this frontend must carry the bounded id and \
                 the accept hint; row={:?} rows={rendered:#?}",
                handoff_row.content()
            );
            let detached_row = &messages[6];
            assert!(
                detached_row.content().contains(&full_attachment_id[..8])
                    && !detached_row.content().contains(&full_attachment_id),
                "INVARIANT: the detached row must render a bounded id fragment, never the \
                 raw full UUID dump; row={:?} rows={rendered:#?}",
                detached_row.content()
            );

            event_loop
                .render_remote_brain_message(crate::brain::BrainWireMessage::Event {
                    event: brain_event(
                        8,
                        "daemon",
                        BrainEventKind::RunStarted {
                            run: crate::brain::BrainRun {
                                run_id,
                                kind: BrainRunKind::Interactive,
                                parent_run_id: None,
                                request_seq: 1,
                                initiating_attachment_id: attachment_id,
                                initiated_by: "shammah@box.local/frontend-cc388712".into(),
                                status: BrainRunStatus::Running,
                                started_ms: 0,
                                updated_ms: 0,
                                detail: None,
                            },
                        },
                    ),
                })
                .await
                .expect("live run event must dispatch through the run projection");
            let after_run = event_loop.output_manager.get_messages();
            let notices = after_run
                .iter()
                .filter(|message| {
                    message.component_view().is_some_and(|view| {
                        matches!(
                            view,
                            finch_ui_model::ComponentView::StaticText(text)
                                if matches!(text.kind, finch_ui_model::StaticTextKind::Info)
                        )
                    })
                })
                .count();
            assert_eq!(
                after_run.len(),
                messages.len() + 1,
                "INVARIANT: a run-status event keeps flowing to the run-group projection, \
                 never a notice row; after={:?}",
                after_run
                    .iter()
                    .map(|message| (message.content(), message.work_unit_head().is_some()))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                notices,
                events.len(),
                "INVARIANT: run-status projection must not add notice rows; notices={notices} \
                 rows={rendered:#?}"
            );
        })
        .await;
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

/// #1387: the exit message used to key off `home_brain.is_some()`, the live
/// watch attachment, instead of whether the Brain was ever durably
/// registered with the daemon this session. Those two go out of sync
/// whenever `register_home_brain` succeeds (creating/loading the Brain's
/// entry in `BrainStore`, exactly what `finch brain ls` later reads) but
/// `attach_home_brain`'s own watch either has not completed yet or dropped
/// afterward — the reported repro's likely path, since the banner and
/// status bar both print `session_label` independently of `home_brain`.
///
/// Before the fix this asserted false: `exit_resume_line()` still read
/// `self.home_brain.is_some()` (`None` here) and printed "This run was not
/// saved as a named Brain and cannot be resumed" for a Brain that
/// `register_home_brain` had already created on disk. After the fix it
/// reads the latched `home_brain_registered` flag instead, so the message
/// agrees with what `finch brain ls` would show.
#[tokio::test]
async fn test_exit_resume_line_reports_saved_when_brain_registered_but_watch_detached() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut event_loop = runner_recovery_test_event_loop();
            assert!(
                event_loop.home_brain.is_none(),
                "fixture precondition: no live watch attachment"
            );

            // Simulate what the real startup path does on a successful
            // `register_home_brain` (event_loop.rs's `Ok(state) => { self
            // .home_brain_registered = state.is_some(); ... }`) without ever
            // reaching a successful `attach_home_brain`, e.g. because the
            // watch attempt is still retrying when `/quit` runs.
            event_loop.home_brain_registered = true;

            let line = event_loop.exit_resume_line();
            assert!(
                !line.contains("not saved") && !line.contains("cannot be resumed"),
                "a Brain the daemon already durably registered must not be reported as unsaved \
                 (home_brain={:?}, home_brain_registered={}), got {line:?}",
                event_loop.home_brain.is_some(),
                event_loop.home_brain_registered,
            );
            assert_eq!(
                line, "To resume, run: finch attach audit-test",
                "a registered Brain must print the real copyable resume command, got {line:?}"
            );
        })
        .await;
}

/// Companion to the above: a session that never reached the daemon at all
/// (`register_home_brain` returned `Ok(None)`, so `home_brain_registered`
/// stays at its default `false`) has genuinely created nothing durable, and
/// the exit line must still say so honestly.
#[tokio::test]
async fn test_exit_resume_line_reports_not_saved_when_registration_never_happened() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let event_loop = runner_recovery_test_event_loop();
            assert!(!event_loop.home_brain_registered, "fixture precondition");
            assert!(event_loop.home_brain.is_none(), "fixture precondition");

            let line = event_loop.exit_resume_line();
            assert!(
                line.contains("not saved") && line.contains("cannot be resumed"),
                "an unregistered session has nothing on disk to resume, got {line:?}"
            );
            assert!(
                !line.contains("finch attach"),
                "an unregistered session must not print a copyable attach command, got {line:?}"
            );
        })
        .await;
}

fn provider_switch_local_entry(
    family: crate::models::ModelFamily,
    size: crate::models::ModelSize,
) -> crate::config::ProviderEntry {
    crate::config::ProviderEntry::Local {
        inference_provider: crate::models::InferenceProvider::LlamaCpp,
        execution_target: crate::config::ExecutionTarget::Auto,
        model_family: family,
        model_size: size,
        model_path: None,
        managed_artifact: None,
        enabled: true,
        name: None,
    }
}

#[tokio::test]
async fn provider_switch_does_not_claim_success_when_daemon_is_ready_with_a_different_local_model()
{
    tokio::task::LocalSet::new()
        .run_until(
            provider_switch_does_not_claim_success_when_daemon_is_ready_with_a_different_local_model_scenario(),
        )
        .await;
}

async fn provider_switch_does_not_claim_success_when_daemon_is_ready_with_a_different_local_model_scenario(
) {
    // Reproduces the reported bug: the daemon already has Gemma loaded and
    // ready (e.g. from an earlier `/provider` activation in this same
    // process), and the user now asks to switch to Qwen — a different local
    // profile. The daemon bootstraps exactly one local model for its whole
    // process lifetime, so it can never actually become ready with Qwen
    // without a restart. `handle_provider_switch` must not read "some local
    // model is ready" as "the requested local entry is ready".
    let mut daemon = mockito::Server::new_async().await;
    let status_mock = daemon
        .mock("GET", "/v1/status")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "generator": {
                    "state": "ready",
                    "model_size": "Gemma 2 9b (LlamaCpp; requested Auto)"
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let gemma = provider_switch_local_entry(
        crate::models::ModelFamily::Gemma2,
        crate::models::ModelSize::Medium,
    );
    let qwen = provider_switch_local_entry(
        crate::models::ModelFamily::Qwen2,
        crate::models::ModelSize::Medium,
    );
    assert_eq!(gemma.profile_name(), "local-gemma-2-9b");
    assert_eq!(qwen.profile_name(), "local-qwen-2.5-3b");

    let daemon_client = Arc::new(crate::client::DaemonClient::for_test(daemon.url()));
    let mut event_loop = super::EventLoop::new_provider_switch_test_runner(
        vec![gemma, qwen],
        0,
        Some(daemon_client),
    );
    event_loop.output_manager.disable_stdout();

    // Requesting profile 2 (Qwen, index 1) while index 0 (Gemma) is active.
    event_loop
        .handle_provider_switch("2".to_string())
        .await
        .expect("provider switch must not error even when it refuses to activate");

    status_mock.assert_async().await;

    let messages: Vec<String> = event_loop
        .output_manager
        .get_messages()
        .iter()
        .map(|message| message.content())
        .collect();
    assert!(
        !messages
            .iter()
            .any(|message| message.contains('✓') && message.contains("local-qwen-2.5-3b")),
        "must not confirm success switching to local-qwen-2.5-3b while the daemon still serves Gemma; messages={messages:?}"
    );
    assert!(
        messages.iter().any(|message| message.contains("local-qwen-2.5-3b")
            && message.contains("Gemma 2 9b (LlamaCpp; requested Auto)")
            && message.contains("restart")),
        "must name both the requested and already-running model and explain a restart is required; messages={messages:?}"
    );
    assert_eq!(
        event_loop.model_selection.active_index().await,
        0,
        "the active generator must stay on the entry the daemon actually serves, not silently move to the mislabeled one"
    );
}

#[tokio::test]
async fn provider_switch_activates_immediately_when_daemon_is_ready_with_the_requested_local_model()
{
    tokio::task::LocalSet::new()
        .run_until(
            provider_switch_activates_immediately_when_daemon_is_ready_with_the_requested_local_model_scenario(),
        )
        .await;
}

async fn provider_switch_activates_immediately_when_daemon_is_ready_with_the_requested_local_model_scenario(
) {
    // The matching case must keep working: when the daemon's already-ready
    // local model is genuinely the one the caller requested, the fast path
    // still activates without waiting.
    let mut daemon = mockito::Server::new_async().await;
    let status_mock = daemon
        .mock("GET", "/v1/status")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "generator": {
                    "state": "ready",
                    "model_size": "Qwen 2.5 3B (LlamaCpp; requested Auto)"
                }
            })
            .to_string(),
        )
        .create_async()
        .await;
    // The matching path persists the new selection on the session's Brain
    // before confirming; without this mock the PUT 404s and the confirmation
    // is replaced by a persistence-failure warning.
    let selection_mock = daemon
        .mock("PUT", "/v1/brains/named/provider-switch-test/selection")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .create_async()
        .await;

    let gemma = provider_switch_local_entry(
        crate::models::ModelFamily::Gemma2,
        crate::models::ModelSize::Medium,
    );
    let qwen = provider_switch_local_entry(
        crate::models::ModelFamily::Qwen2,
        crate::models::ModelSize::Medium,
    );

    let daemon_client = Arc::new(crate::client::DaemonClient::for_test(daemon.url()));
    let mut event_loop = super::EventLoop::new_provider_switch_test_runner(
        vec![gemma, qwen],
        0,
        Some(daemon_client),
    );
    event_loop.output_manager.disable_stdout();

    event_loop
        .handle_provider_switch("2".to_string())
        .await
        .expect("provider switch to a matching ready local model must succeed");

    status_mock.assert_async().await;
    selection_mock.assert_async().await;

    let messages: Vec<String> = event_loop
        .output_manager
        .get_messages()
        .iter()
        .map(|message| message.content())
        .collect();
    assert!(
        messages.iter().any(|message| message.contains('✓')
            && message.contains("local-qwen-2.5-3b")
            && message.contains("Qwen 2.5 3B (LlamaCpp; requested Auto)")),
        "a genuinely matching ready local model must still confirm the switch; messages={messages:?}"
    );
    assert_eq!(
        event_loop.model_selection.active_index().await,
        1,
        "a genuinely matching ready local model must activate immediately"
    );
}

#[tokio::test]
async fn hydrate_brain_selection_fails_closed_when_daemon_already_runs_a_different_local_model() {
    tokio::task::LocalSet::new()
        .run_until(
            hydrate_brain_selection_fails_closed_when_daemon_already_runs_a_different_local_model_scenario(),
        )
        .await;
}

async fn hydrate_brain_selection_fails_closed_when_daemon_already_runs_a_different_local_model_scenario(
) {
    // The same wrong-model bug reachable through session startup: a Brain
    // persisted with `local-gemma-2-9b` reattaches to a daemon whose one
    // local model slot is already Ready with Qwen (e.g. loaded once at that
    // daemon's own startup, unrelated to what any Brain last persisted).
    // `hydrate_brain_selection` -> `apply_effective_selection` must not
    // silently activate a generator labeled Gemma while the daemon keeps
    // serving Qwen.
    let mut daemon = mockito::Server::new_async().await;
    let selection_mock = daemon
        .mock("GET", "/v1/brains/named/provider-switch-test/selection")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::json!({"provider": "local-gemma-2-9b"}).to_string())
        .create_async()
        .await;
    let status_mock = daemon
        .mock("GET", "/v1/status")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "generator": {
                    "state": "ready",
                    "model_size": "Qwen 2.5 3B (LlamaCpp; requested Auto)"
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let gemma = provider_switch_local_entry(
        crate::models::ModelFamily::Gemma2,
        crate::models::ModelSize::Medium,
    );
    let qwen = provider_switch_local_entry(
        crate::models::ModelFamily::Qwen2,
        crate::models::ModelSize::Medium,
    );

    let daemon_client = Arc::new(crate::client::DaemonClient::for_test(daemon.url()));
    let mut event_loop = super::EventLoop::new_provider_switch_test_runner(
        vec![gemma, qwen],
        1,
        Some(daemon_client),
    );
    event_loop.output_manager.disable_stdout();

    let result = event_loop.hydrate_brain_selection().await;

    selection_mock.assert_async().await;
    status_mock.assert_async().await;
    let error = result.expect_err(
        "reattaching to a daemon already running a different local model must fail closed, not silently activate the mislabeled one",
    );
    let message = error.to_string();
    assert!(
        message.contains("local-gemma-2-9b") && message.contains("Qwen 2.5 3B"),
        "error must name both the persisted entry and the model the daemon actually reports; message={message:?}"
    );
    assert_eq!(
        event_loop.model_selection.active_index().await,
        1,
        "the active generator must be untouched when activation is refused"
    );
}

/// #1318: on a fresh session, the bottom status rule (`status_rule_line` in
/// `crates/finch-tui/src/lib.rs`) rendered as a blank line of dashes with no
/// provider/model text — reproduced live, persisting through multiple
/// completed turns. `EventLoop::project_model_identity` (called by
/// `hydrate_brain_selection` -> `apply_effective_selection` on every
/// startup) used `tui_renderer.try_lock()`; if `async_input::spawn_input_task`'s
/// own periodic `tui_renderer.lock()` (`crates/finch-tui/src/async_input.rs`,
/// polling `crossterm::event::poll` under the lock) held the mutex at that
/// exact moment, the `try_lock` failed silently and nothing ever retried, so
/// `model_identity` stayed empty for the rest of the session. This predates
/// and is unrelated to #1313/#1314/#1315 (none of those PRs touch
/// `model_identity`, `project_model_identity`, or `status_rule_line`); it is
/// a pre-existing race from #988 that a live session happened to lose.
///
/// This test forces the contention deterministically — holding the renderer
/// lock across the entire `hydrate_brain_selection` call, standing in for
/// the input task's hold — so it fails reliably against the pre-fix
/// `try_lock` and passes reliably once `project_model_identity` blocks on
/// the lock instead. `status_rule_never_wraps_and_keeps_identity_on_the_left`
/// in `crates/finch-tui/src/lib.rs` separately pins that a populated
/// `model_identity` always renders as visible divider text; this test pins
/// that real startup hydration actually populates it under contention.
#[tokio::test]
async fn hydrate_brain_selection_sets_model_identity_despite_renderer_lock_contention() {
    tokio::task::LocalSet::new()
        .run_until(
            hydrate_brain_selection_sets_model_identity_despite_renderer_lock_contention_scenario(),
        )
        .await;
}

async fn hydrate_brain_selection_sets_model_identity_despite_renderer_lock_contention_scenario() {
    let mut daemon = mockito::Server::new_async().await;
    let selection_mock = daemon
        .mock("GET", "/v1/brains/named/provider-switch-test/selection")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::json!({}).to_string())
        .create_async()
        .await;
    let status_mock = daemon
        .mock("GET", "/v1/status")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "generator": {
                    "state": "ready",
                    "model_size": "Gemma 2 9b (LlamaCpp; requested Auto)"
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let gemma = provider_switch_local_entry(
        crate::models::ModelFamily::Gemma2,
        crate::models::ModelSize::Medium,
    );
    let expected_display_name = gemma.display_name().to_string();

    let daemon_client = Arc::new(crate::client::DaemonClient::for_test(daemon.url()));
    let mut event_loop =
        super::EventLoop::new_provider_switch_test_runner(vec![gemma], 0, Some(daemon_client));
    event_loop.output_manager.disable_stdout();

    // Take the renderer lock up front (as an owned guard so it can move into
    // a spawned task) and only release it well after `hydrate_brain_selection`
    // should have finished, so every lock acquisition inside it — including
    // the fixed `project_model_identity` — must wait rather than get lucky.
    let guard = Arc::clone(&event_loop.tui_renderer).lock_owned().await;
    let hold_task = tokio::task::spawn_local(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        drop(guard);
    });

    event_loop
        .hydrate_brain_selection()
        .await
        .expect("a fresh Brain with a matching ready local model must hydrate cleanly");

    hold_task
        .await
        .expect("the simulated input-task lock hold must not panic");

    selection_mock.assert_async().await;
    status_mock.assert_async().await;

    let identity = event_loop
        .tui_renderer
        .lock()
        .await
        .model_identity()
        .to_string();
    assert!(
        identity.contains(&expected_display_name) && identity.contains("local"),
        "the bottom status rule's provider/model identity must be set even when \
         startup hydration had to wait out a contended renderer lock (#1318 — a lost \
         try_lock race left this permanently blank); expected_display_name={expected_display_name:?}, \
         identity={identity:?}"
    );
}

#[tokio::test]
async fn provider_list_shows_each_local_entry_its_own_model_not_the_bare_local_tag() {
    tokio::task::LocalSet::new()
        .run_until(
            provider_list_shows_each_local_entry_its_own_model_not_the_bare_local_tag_scenario(),
        )
        .await;
}

async fn provider_list_shows_each_local_entry_its_own_model_not_the_bare_local_tag_scenario() {
    // Reproduces the reported bug: two distinct local entries (Gemma 2 9B and
    // Qwen 2.5 3B) both rendered their trailing descriptor as the literal
    // string "local" — `ProviderEntry::model()` is documented cloud-only and
    // returns `None` for every `Local` variant, so `/providers` fell back to
    // `provider_type()`, which is the same "local" tag for both, making two
    // genuinely different models look identically labeled. The cloud entry
    // must keep showing its real model string unaffected.
    let gemma = provider_switch_local_entry(
        crate::models::ModelFamily::Gemma2,
        crate::models::ModelSize::Medium,
    );
    let qwen = provider_switch_local_entry(
        crate::models::ModelFamily::Qwen2,
        crate::models::ModelSize::Medium,
    );
    let chatgpt = crate::config::ProviderEntry::Openai {
        api_key: "test-key".to_string(),
        model: Some("gpt-5.6-sol".to_string()),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some("ChatGPT Personal".to_string()),
        reasoning_effort: None,
    };

    let mut event_loop =
        super::EventLoop::new_provider_switch_test_runner(vec![gemma, chatgpt, qwen], 0, None);
    event_loop.output_manager.disable_stdout();

    event_loop
        .handle_provider_list()
        .await
        .expect("listing configured providers must not error");

    let messages: Vec<String> = event_loop
        .output_manager
        .get_messages()
        .iter()
        .map(|message| message.content())
        .collect();
    let listing = messages
        .iter()
        .find(|message| message.contains("Configured provider entries:"))
        .unwrap_or_else(|| panic!("expected a provider listing message; messages={messages:?}"));

    assert!(
        listing.contains("[local]") && listing.contains("Gemma 2 9b"),
        "Gemma entry must show its own model descriptor, not the bare 'local' tag; listing={listing:?}"
    );
    assert!(
        listing.contains("[local]") && listing.contains("Qwen 2.5 3B"),
        "Qwen entry must show its own model descriptor, not the bare 'local' tag; listing={listing:?}"
    );
    assert!(
        !listing.contains("· local\n") && !listing.ends_with("· local"),
        "no line may fall back to the bare literal 'local' descriptor now that both local entries have real model info; listing={listing:?}"
    );
    assert!(
        listing.contains("[cloud]") && listing.contains("ChatGPT Personal") && listing.contains("gpt-5.6-sol"),
        "the cloud entry's real model must remain unaffected by the local-descriptor fix; listing={listing:?}"
    );
}

#[tokio::test]
async fn status_reports_generic_compatible_capabilities_without_secrets_and_preserves_builtins() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let secret_markers = [
                "https://private-compatible.example/v1",
                "/private/chat/completions",
                "/private/models",
                "private-credential-ref",
                "PRIVATE_COMPATIBLE_API_KEY",
                "resolved-secret-value",
                "X-Private-Header",
                "https://private-audience.example",
            ];
            let compatible = crate::config::ProviderEntry::OpenAiCompatible {
                name: "compatible-work".into(),
                base_url: secret_markers[0].into(),
                chat_path: Some(secret_markers[1].into()),
                models_path: Some(secret_markers[2].into()),
                model: "main".into(),
                credential: crate::config::CredentialBinding {
                    credential_ref: secret_markers[3].into(),
                    audience: Some(crate::config::AudienceBinding {
                        family: crate::config::EndpointFamily::Custom,
                        endpoint: Some(secret_markers[7].into()),
                    }),
                    tenant: Some(secret_markers[5].into()),
                    project: Some(secret_markers[6].into()),
                    account: Some(secret_markers[4].into()),
                    required_scopes: Default::default(),
                },
                capabilities: crate::config::OpenAiCompatibleCapabilities {
                    streaming: Some(true),
                    tools: Some(true),
                    parallel_tool_calls: Some(false),
                    image_input: Some(false),
                    context_window_tokens: Some(262_144),
                    max_output_tokens: Some(32_768),
                },
                tool_choice: crate::config::OpenAiCompatibleToolChoice::Auto,
                strict_tool_schemas: Some(false),
            };
            let overlay_compatible = compatible.clone();
            let mut event_loop = super::EventLoop::new_provider_switch_test_runner(
                vec![compatible],
                0,
                None,
            );
            event_loop.output_manager.disable_stdout();

            event_loop
                .handle_user_input("/status".into())
                .await
                .expect("the real /status command path must project compatible diagnostics");

            let messages: Vec<String> = event_loop
                .output_manager
                .get_messages()
                .iter()
                .map(|message| message.content())
                .collect();
            let report = messages
                .iter()
                .cloned()
                .find(|message| message.starts_with("provider: compatible-work\n"))
                .unwrap_or_else(|| {
                    panic!(
                        "the real /status path must emit the selected profile report; messages={messages:?}"
                    )
                });
            let expected = "provider: compatible-work\n\
model: main\n\
thinking: provider default\n\
source: inherited global default\n\
dialect: generic OpenAI-compatible Chat Completions\n\
context window: 262144 tokens (operator configured)\n\
max output: 32768 tokens (operator configured)\n\
image input: unsupported (text only; operator configured)\n\
capacity: not configured";
            assert_eq!(
                report, expected,
                "generic compatible /status must identify the dialect and report only operator-attested, secret-free capability diagnostics"
            );
            assert!(
                !report.contains("provider: openai\n") && !report.contains("provider: OpenAI\n"),
                "a generic compatible profile must not be identified as OpenAI; report={report:?}"
            );
            for marker in secret_markers {
                assert!(
                    !report.contains(marker),
                    "/status must not expose endpoint, path, credential, environment, header, or resolved-secret material; marker={marker:?} report={report:?}"
                );
            }

            let mut overlay = super::EventLoop::new_provider_switch_test_runner(
                vec![overlay_compatible],
                0,
                None,
            );
            overlay.output_manager.disable_stdout();
            overlay.cli_model = Some("alternate".into());
            overlay
                .handle_user_input("/status".into())
                .await
                .expect("the real /status command path must handle a one-shot model overlay");
            let overlay_messages: Vec<String> = overlay
                .output_manager
                .get_messages()
                .iter()
                .map(|message| message.content())
                .collect();
            let overlay_report = overlay_messages
                .iter()
                .find(|message| message.starts_with("provider: compatible-work\n"))
                .unwrap_or_else(|| {
                    panic!(
                        "the real /status path must emit the overlaid profile report; messages={overlay_messages:?}"
                    )
                });
            let expected_overlay = "provider: compatible-work\n\
model: alternate\n\
thinking: provider default\n\
source: CLI --model (this invocation only)\n\
dialect: generic OpenAI-compatible Chat Completions\n\
capabilities: not attested for model overlay 'alternate' (configured model: main)\n\
context window: not attested for selected model\n\
max output: not attested for selected model\n\
image input: not attested for selected model\n\
capacity: not configured";
            assert_eq!(
                overlay_report, expected_overlay,
                "a model overlay must clearly withhold capability values attested only for the configured compatible model"
            );
            for configured_model_claim in [
                "262144",
                "32768",
                "text only; operator configured",
                "supported (operator configured)",
            ] {
                assert!(
                    !overlay_report.contains(configured_model_claim),
                    "an overlaid model must not inherit the configured model's capability claim; claim={configured_model_claim:?} report={overlay_report:?}"
                );
            }
            for marker in secret_markers {
                assert!(
                    !overlay_report.contains(marker),
                    "the overlay provenance diagnostic must remain secret-free; marker={marker:?} report={overlay_report:?}"
                );
            }

            for (entry, expected) in [
                (
                    crate::config::ProviderEntry::Openai {
                        api_key: "sk-built-in-control".into(),
                        model: Some("gpt-5".into()),
                        base_url: None,
                        chat_path: None,
                        models_path: None,
                        name: Some("official-openai".into()),
                        reasoning_effort: None,
                    },
                    "provider: official-openai\nmodel: gpt-5\nthinking: provider default\nsource: inherited global default",
                ),
                (
                    crate::config::ProviderEntry::Claude {
                        api_key: "sk-ant-built-in-control".into(),
                        model: Some("claude-sonnet-5".into()),
                        base_url: None,
                        chat_path: None,
                        models_path: None,
                        name: Some("official-claude".into()),
                    },
                    "provider: official-claude\nmodel: claude-sonnet-5\nthinking: provider default\nsource: inherited global default",
                ),
            ] {
                let mut control = super::EventLoop::new_provider_switch_test_runner(
                    vec![entry],
                    0,
                    None,
                );
                control.output_manager.disable_stdout();
                control
                    .handle_user_input("/status".into())
                    .await
                    .expect("built-in provider /status control must remain available");
                let messages: Vec<String> = control
                    .output_manager
                    .get_messages()
                    .iter()
                    .map(|message| message.content())
                    .collect();
                let report = messages
                    .iter()
                    .cloned()
                    .find(|message| message.starts_with("provider: "))
                    .unwrap_or_else(|| {
                        panic!("built-in /status must emit its existing report; messages={messages:?}")
                    });
                assert_eq!(
                    report, expected,
                    "generic-compatible diagnostics must not alter built-in provider status output"
                );
            }
        })
        .await;
}

#[tokio::test]
async fn provider_switch_activates_configured_subscription_provider() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let entry = crate::config::ProviderEntry::Credentialed {
                provider: crate::config::CredentialProvider::ChatgptSubscription,
                credential: crate::config::CredentialBinding {
                    credential_ref: "chatgpt-sub".into(),
                    audience: None,
                    tenant: None,
                    project: None,
                    account: Some("user@example.com".into()),
                    required_scopes: std::collections::BTreeSet::new(),
                },
                model: Some("gpt-5.6-sol".into()),
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("ChatGPT Subscription".into()),
                reasoning_effort: None,
            };
            let credential = crate::config::ProviderCredential {
                name: "chatgpt-sub".into(),
                kind: crate::config::CredentialKind::Bearer,
                provider: crate::config::CredentialProvider::ChatgptSubscription,
                issuer: "openai-chatgpt".into(),
                audience: crate::config::AudienceBinding::standard(
                    crate::config::EndpointFamily::ChatgptSubscription,
                ),
                tenant: None,
                project: None,
                account: Some("user@example.com".into()),
                scopes: std::collections::BTreeSet::new(),
                secret_ref: "oauth-store:chatgpt-sub".into(),
                lifecycle: crate::config::CredentialLifecycle::default(),
                revocation: Default::default(),
            };
            let initial = crate::config::ProviderEntry::Openai {
                api_key: "sk-test".into(),
                model: Some("gpt-4o".into()),
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("Initial OpenAI".into()),
                reasoning_effort: None,
            };
            let config = crate::config::Config::with_providers(vec![initial, entry])
                .with_credentials(vec![credential]);

            let mut event_loop =
                super::EventLoop::new_provider_switch_test_runner_with_config(config, 0, None);
            event_loop.output_manager.disable_stdout();

            event_loop
                .handle_provider_switch("2".to_string())
                .await
                .expect("provider switch to subscription must not error");

            let messages: Vec<String> = event_loop
                .output_manager
                .get_messages()
                .iter()
                .map(|message| message.content())
                .collect();
            assert!(
                !messages
                    .iter()
                    .any(|m| m.contains("Injected credential resolvers cannot fabricate")),
                "switching to subscription provider must not hit injected credential resolver bail; messages={messages:?}"
            );
            assert!(
                !messages
                    .iter()
                    .any(|m| m.contains("Failed to create model")),
                "switching to subscription provider must not fail model creation; messages={messages:?}"
            );
            assert_eq!(
                event_loop.model_selection.active_index().await,
                1,
                "the active generator must switch to the subscription entry index"
            );
        })
        .await;
}

// ---------------------------------------------------------------------------
// `/patterns` commands (issue #1634: the commands for reviewing and revoking
// standing tool approvals printed "recognized but not yet implemented" in the
// live session although the approvals themselves were stored and matched).
//
// Every test below drives `EventLoop::handle_user_input` (the real command
// dispatch) and `EventLoop::resolve_dialog_result` (the real dialog-answer
// routing), and decides "does this still auto-approve?" by spawning a real
// tool call through the event loop's own `ToolExecutionCoordinator`, the
// production approval path. The pattern store lives in a temporary directory.
// ---------------------------------------------------------------------------

/// What the production approval path did with one tool call.
#[derive(Debug, PartialEq)]
enum ApprovalProbe {
    /// The call ran without asking: a standing approval matched it.
    AutoApproved,
    /// The call raised `ToolApprovalNeeded`: the owner is asked.
    AskedOwner,
}

fn patterns_event_loop() -> (super::EventLoop, tempfile::TempDir, std::path::PathBuf) {
    let (event_loop, tempdir) = auto_accept_event_loop();
    event_loop.output_manager.disable_stdout();
    let store_path = tempdir.path().join("patterns.json");
    (event_loop, tempdir, store_path)
}

fn bash_call(command: &str) -> crate::tools::ToolUse {
    crate::tools::ToolUse::new(
        "bash".to_string(),
        serde_json::json!({ "command": command }),
    )
}

/// A wildcard pattern whose text is exactly the signature of `command`, so
/// it matches that call and nothing else.
fn pattern_for_bash(command: &str) -> crate::tools::ToolPattern {
    let signature =
        crate::tools::generate_tool_signature(&bash_call(command), std::path::Path::new("."));
    crate::tools::ToolPattern::new(
        signature.context_key,
        "bash".to_string(),
        format!("allow {command}"),
    )
}

/// Send one bash call through the event loop's real tool coordinator and
/// report whether the approval path let it run or asked the owner.
async fn probe_bash_approval(event_loop: &mut super::EventLoop, command: &str) -> ApprovalProbe {
    let query_id = Uuid::new_v4();
    let tool_use = bash_call(command);
    let tool_id = tool_use.id.clone();
    let round_token = crate::cli::conversation::ConversationHistory::new()
        .stage_assistant(
            query_id,
            crate::providers::Message {
                role: "assistant".into(),
                content: vec![crate::providers::ContentBlock::ToolUse {
                    id: tool_id.clone(),
                    name: tool_use.name.clone(),
                    input: tool_use.input.clone(),
                }],
            },
        )
        .expect("mint a tool round token for the probe");
    let work_unit = event_loop.output_manager.start_work_unit("Tools");
    let row_idx = work_unit.add_row(format!("bash({command})"));
    event_loop.tool_coordinator.spawn_tool_execution(
        query_id,
        round_token,
        tool_use,
        work_unit,
        row_idx,
        None,
        None,
    );
    loop {
        let event = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            event_loop.event_rx.recv(),
        )
        .await
        .expect("the approval probe hung: the tool coordinator emitted neither an approval request nor a result")
        .expect("the event channel must stay open during the probe");
        match event {
            ReplEvent::ToolApprovalNeeded { tool_use, .. } if tool_use.id == tool_id => {
                return ApprovalProbe::AskedOwner;
            }
            ReplEvent::ToolResult { tool_id: id, .. } if id == tool_id => {
                return ApprovalProbe::AutoApproved;
            }
            _ => {}
        }
    }
}

fn scrollback(event_loop: &super::EventLoop) -> Vec<String> {
    event_loop
        .output_manager
        .get_messages()
        .iter()
        .map(|message| message.content())
        .collect()
}

/// Answer the dialog on screen the way the input task does: the dialog
/// leaves the screen, then its result is routed.
async fn answer_dialog(
    event_loop: &mut super::EventLoop,
    result: crate::cli::tui::DialogResult,
    step: &str,
) {
    let dialog = event_loop.tui_renderer.lock().await.active_dialog.take();
    assert!(
        dialog.is_some(),
        "a dialog must be on screen before it can be answered; step={step} scrollback={:?}",
        scrollback(event_loop)
    );
    event_loop
        .resolve_dialog_result(result)
        .await
        .expect("routing a dialog answer must succeed");
}

async fn stored_ids(event_loop: &super::EventLoop) -> (Vec<String>, Vec<String>) {
    let executor = event_loop.tool_coordinator.tool_executor().lock().await;
    let store = executor.persistent_store();
    (
        store.patterns.iter().map(|p| p.id.clone()).collect(),
        store.exact_approvals.iter().map(|a| a.id.clone()).collect(),
    )
}

fn ids_on_disk(path: &std::path::Path) -> (Vec<String>, Vec<String>) {
    let store = crate::tools::PersistentPatternStore::load(path)
        .expect("the pattern store file must be readable");
    (
        store.patterns.iter().map(|p| p.id.clone()).collect(),
        store.exact_approvals.iter().map(|a| a.id.clone()).collect(),
    )
}

#[tokio::test]
async fn test_patterns_list_shows_id_match_scope_and_count_from_the_live_approval_store() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir, _store_path) = patterns_event_loop();
            let persistent = pattern_for_bash("cargo build --offline");
            let session = pattern_for_bash("cargo fmt --check");
            let exact = crate::tools::generate_tool_signature(
                &bash_call("git status --short"),
                std::path::Path::new("."),
            );
            {
                let mut executor = event_loop.tool_coordinator.tool_executor().lock().await;
                executor.approve_pattern_persistent(persistent.clone());
                executor.approve_pattern_session(session.clone());
                executor.approve_exact_persistent(exact.clone());
                executor.save_patterns().expect("seed the temporary store");
            }
            // One real auto-approved call, so the count shown is the count
            // the approval path itself recorded.
            assert_eq!(
                probe_bash_approval(&mut event_loop, "cargo build --offline").await,
                ApprovalProbe::AutoApproved,
                "fixture: the seeded persistent pattern must auto-approve its command"
            );

            for command in ["/patterns list", "/patterns"] {
                let before = scrollback(&event_loop).len();
                event_loop
                    .handle_user_input(command.to_string())
                    .await
                    .expect("the list command must dispatch");
                let rows = scrollback(&event_loop)[before..].to_vec();
                let listing = rows.join("\n");
                assert!(
                    !listing.contains("not yet implemented"),
                    "{command} must list the approvals, not report itself unimplemented; rows={rows:?}"
                );
                for (what, expected) in [
                    ("the persistent pattern's short ID and scope", format!("{}  persistent  wildcard", &persistent.id[..8])),
                    ("what the persistent pattern matches", format!("Matches: {}", persistent.pattern)),
                    ("the persistent pattern's recorded match count", "Match count: 1 |".to_string()),
                    ("the session pattern's short ID and scope", format!("{}  session  wildcard", &session.id[..8])),
                    ("what the session pattern matches", format!("Matches: {}", session.pattern)),
                    ("the session pattern's match count", "Match count: 0 |".to_string()),
                    ("what the exact approval matches", format!("Matches: {}", exact.context_key)),
                    ("the totals", "Total: 2 patterns (1 persistent, 1 session), 1 exact approvals (1 persistent, 0 session)".to_string()),
                ] {
                    assert!(
                        listing.contains(&expected),
                        "{command} must show {what}; expected={expected:?} rows={rows:?}"
                    );
                }
                assert!(
                    !listing.contains('\u{1b}'),
                    "the listing must be plain text with no terminal styling; rows={rows:?}"
                );
            }
        })
        .await;
}

#[tokio::test]
async fn test_patterns_remove_revokes_a_persistent_pattern_so_the_next_call_asks() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir, store_path) = patterns_event_loop();
            let command = "cargo build --offline";
            assert_eq!(
                probe_bash_approval(&mut event_loop, command).await,
                ApprovalProbe::AskedOwner,
                "fixture: with no standing approval the call must ask the owner"
            );
            let pattern = pattern_for_bash(command);
            let kept = pattern_for_bash("cargo fmt --check");
            {
                let mut executor = event_loop.tool_coordinator.tool_executor().lock().await;
                executor.approve_pattern_persistent(pattern.clone());
                executor.approve_pattern_persistent(kept.clone());
                executor.save_patterns().expect("seed the temporary store");
            }
            assert_eq!(
                probe_bash_approval(&mut event_loop, command).await,
                ApprovalProbe::AutoApproved,
                "fixture: the persistent pattern must auto-approve its command before removal"
            );

            // An ID that names nothing changes nothing.
            event_loop
                .handle_user_input("/patterns remove 00000000-not-a-real-id".to_string())
                .await
                .expect("removing an unknown ID must dispatch");
            assert_eq!(
                stored_ids(&event_loop).await.0,
                vec![pattern.id.clone(), kept.id.clone()],
                "an unknown ID must leave the store unchanged; scrollback={:?}",
                scrollback(&event_loop)
            );
            assert!(
                scrollback(&event_loop)
                    .iter()
                    .any(|row| row.contains("No pattern or approval found with ID: 00000000-not-a-real-id")),
                "an unknown ID must be named in the reply; scrollback={:?}",
                scrollback(&event_loop)
            );

            // The short ID shown by `/patterns list` is enough.
            event_loop
                .handle_user_input(format!("/patterns remove {}", &pattern.id[..8]))
                .await
                .expect("the remove command must dispatch");

            let rows = scrollback(&event_loop);
            assert!(
                rows.iter()
                    .any(|row| row.contains(&format!("Removed pattern: {}", &pattern.id[..8]))),
                "remove must confirm what it removed, not report itself unimplemented; scrollback={rows:?}"
            );
            assert_eq!(
                stored_ids(&event_loop).await.0,
                vec![kept.id.clone()],
                "remove must delete exactly the named pattern from the live store; scrollback={rows:?}"
            );
            assert_eq!(
                ids_on_disk(&store_path).0,
                vec![kept.id.clone()],
                "remove must persist, or the pattern would return on restart; scrollback={rows:?}"
            );
            assert_eq!(
                probe_bash_approval(&mut event_loop, command).await,
                ApprovalProbe::AskedOwner,
                "a removed pattern must stop auto-approving in the same session; scrollback={rows:?}"
            );
            assert_eq!(
                probe_bash_approval(&mut event_loop, "cargo fmt --check").await,
                ApprovalProbe::AutoApproved,
                "removing one pattern must not revoke another; scrollback={rows:?}"
            );
        })
        .await;
}

#[tokio::test]
async fn test_patterns_remove_revokes_a_session_pattern_so_the_next_call_asks() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir, _store_path) = patterns_event_loop();
            let command = "cargo fmt --check";
            let pattern = pattern_for_bash(command);
            event_loop
                .tool_coordinator
                .tool_executor()
                .lock()
                .await
                .approve_pattern_session(pattern.clone());
            assert_eq!(
                probe_bash_approval(&mut event_loop, command).await,
                ApprovalProbe::AutoApproved,
                "fixture: the session pattern must auto-approve its command before removal"
            );

            event_loop
                .handle_user_input(format!("/patterns rm {}", pattern.id))
                .await
                .expect("the remove alias must dispatch");

            let rows = scrollback(&event_loop);
            assert!(
                rows.iter()
                    .any(|row| row.contains(&format!("Removed pattern: {}", &pattern.id[..8]))),
                "remove must confirm removal of a session pattern; scrollback={rows:?}"
            );
            assert_eq!(
                probe_bash_approval(&mut event_loop, command).await,
                ApprovalProbe::AskedOwner,
                "a removed session pattern must stop auto-approving in the same session; scrollback={rows:?}"
            );
        })
        .await;
}

#[tokio::test]
async fn test_patterns_remove_of_a_heavily_used_pattern_waits_for_confirmation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir, store_path) = patterns_event_loop();
            let command = "cargo build --offline";
            let mut pattern = pattern_for_bash(command);
            pattern.match_count = 11;
            {
                let mut executor = event_loop.tool_coordinator.tool_executor().lock().await;
                executor.approve_pattern_persistent(pattern.clone());
                executor.save_patterns().expect("seed the temporary store");
            }
            let remove = format!("/patterns remove {}", pattern.id);

            event_loop
                .handle_user_input(remove.clone())
                .await
                .expect("the remove command must dispatch");
            assert_eq!(
                stored_ids(&event_loop).await.0,
                vec![pattern.id.clone()],
                "a pattern used more than ten times must not be removed before the owner confirms; scrollback={:?}",
                scrollback(&event_loop)
            );
            answer_dialog(
                &mut event_loop,
                crate::cli::tui::DialogResult::Confirmed(false),
                "decline removal",
            )
            .await;
            assert_eq!(
                (stored_ids(&event_loop).await.0, ids_on_disk(&store_path).0),
                (vec![pattern.id.clone()], vec![pattern.id.clone()]),
                "a declined removal must leave the live store and the file unchanged; scrollback={:?}",
                scrollback(&event_loop)
            );
            assert_eq!(
                probe_bash_approval(&mut event_loop, command).await,
                ApprovalProbe::AutoApproved,
                "a declined removal must leave the pattern in force"
            );

            event_loop
                .handle_user_input(remove)
                .await
                .expect("the remove command must dispatch a second time");
            answer_dialog(
                &mut event_loop,
                crate::cli::tui::DialogResult::Confirmed(true),
                "confirm removal",
            )
            .await;
            assert_eq!(
                (stored_ids(&event_loop).await.0, ids_on_disk(&store_path).0),
                (Vec::<String>::new(), Vec::<String>::new()),
                "a confirmed removal must delete the pattern from the live store and the file; scrollback={:?}",
                scrollback(&event_loop)
            );
            assert_eq!(
                probe_bash_approval(&mut event_loop, command).await,
                ApprovalProbe::AskedOwner,
                "after a confirmed removal the call must ask the owner again"
            );
        })
        .await;
}

#[tokio::test]
async fn test_patterns_clear_confirms_then_revokes_every_persistent_and_session_approval() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir, store_path) = patterns_event_loop();
            let commands = [
                "cargo build --offline", // persistent pattern
                "cargo fmt --check",     // session pattern
                "git status --short",    // persistent exact approval
                "git diff --stat",       // session exact approval
            ];
            let signature = |command: &str| {
                crate::tools::generate_tool_signature(&bash_call(command), std::path::Path::new("."))
            };
            {
                let mut executor = event_loop.tool_coordinator.tool_executor().lock().await;
                executor.approve_pattern_persistent(pattern_for_bash(commands[0]));
                executor.approve_pattern_session(pattern_for_bash(commands[1]));
                executor.approve_exact_persistent(signature(commands[2]));
                executor.approve_exact_session(signature(commands[3]));
                executor.save_patterns().expect("seed the temporary store");
            }
            for command in commands {
                assert_eq!(
                    probe_bash_approval(&mut event_loop, command).await,
                    ApprovalProbe::AutoApproved,
                    "fixture: every seeded approval must auto-approve before clear; command={command}"
                );
            }
            let seeded = stored_ids(&event_loop).await;

            event_loop
                .handle_user_input("/patterns clear".to_string())
                .await
                .expect("the clear command must dispatch");
            assert!(
                scrollback(&event_loop)
                    .iter()
                    .any(|row| row.contains("This will remove 2 pattern(s) and 2 exact approval(s)")),
                "clear must say what it is about to remove, not report itself unimplemented; scrollback={:?}",
                scrollback(&event_loop)
            );
            assert_eq!(
                stored_ids(&event_loop).await,
                seeded,
                "clear must not change the store before the owner confirms"
            );
            answer_dialog(
                &mut event_loop,
                crate::cli::tui::DialogResult::Cancelled,
                "escape the clear confirmation",
            )
            .await;
            assert_eq!(
                (stored_ids(&event_loop).await, ids_on_disk(&store_path)),
                (seeded.clone(), seeded.clone()),
                "an escaped clear must leave the live store and the file unchanged; scrollback={:?}",
                scrollback(&event_loop)
            );

            event_loop
                .handle_user_input("/patterns clear".to_string())
                .await
                .expect("the clear command must dispatch a second time");
            answer_dialog(
                &mut event_loop,
                crate::cli::tui::DialogResult::Confirmed(true),
                "confirm clear",
            )
            .await;

            let rows = scrollback(&event_loop);
            assert!(
                rows.iter()
                    .any(|row| row.contains("Cleared 4 pattern(s) and approval(s).")),
                "a confirmed clear must report how much it removed; scrollback={rows:?}"
            );
            let empty = (Vec::<String>::new(), Vec::<String>::new());
            assert_eq!(
                (stored_ids(&event_loop).await, ids_on_disk(&store_path)),
                (empty.clone(), empty),
                "a confirmed clear must empty the live store and the file; scrollback={rows:?}"
            );
            for command in commands {
                assert_eq!(
                    probe_bash_approval(&mut event_loop, command).await,
                    ApprovalProbe::AskedOwner,
                    "after clear no standing approval, persistent or session, may auto-approve; command={command} scrollback={rows:?}"
                );
            }

            event_loop
                .handle_user_input("/patterns clear".to_string())
                .await
                .expect("clear on an empty store must dispatch");
            assert!(
                scrollback(&event_loop)
                    .last()
                    .is_some_and(|row| row.contains("No patterns to clear.")),
                "clear on an empty store must say so; scrollback={:?}",
                scrollback(&event_loop)
            );
            assert!(
                event_loop.tui_renderer.lock().await.active_dialog.is_none(),
                "clear on an empty store must not open a confirmation dialog"
            );
        })
        .await;
}

#[tokio::test]
async fn test_patterns_add_wizard_saves_a_persistent_pattern_that_auto_approves() {
    use crate::cli::tui::DialogResult;

    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir, store_path) = patterns_event_loop();
            let command = "cargo build --offline";
            let pattern_text = pattern_for_bash(command).pattern;
            assert_eq!(
                probe_bash_approval(&mut event_loop, command).await,
                ApprovalProbe::AskedOwner,
                "fixture: with no standing approval the call must ask the owner"
            );

            // Escaping the wizard part-way stores nothing.
            event_loop
                .handle_user_input("/patterns add".to_string())
                .await
                .expect("the add command must dispatch");
            answer_dialog(&mut event_loop, DialogResult::Selected(0), "pattern type").await;
            answer_dialog(&mut event_loop, DialogResult::Cancelled, "escape at tool name").await;
            assert_eq!(
                (stored_ids(&event_loop).await.0.len(), store_path.exists()),
                (0, false),
                "an escaped add wizard must store nothing; scrollback={:?}",
                scrollback(&event_loop)
            );
            assert!(
                event_loop.tui_renderer.lock().await.active_dialog.is_none(),
                "an escaped add wizard must not leave a dialog open"
            );

            // An invalid regex is refused before anything is stored.
            event_loop
                .handle_user_input("/patterns add".to_string())
                .await
                .expect("the add command must dispatch");
            answer_dialog(&mut event_loop, DialogResult::Selected(1), "regex type").await;
            for (answer, step) in [("bash", "tool name"), ("(unclosed", "pattern"), ("bad", "description")] {
                answer_dialog(&mut event_loop, DialogResult::TextEntered(answer.to_string()), step)
                    .await;
            }
            assert!(
                scrollback(&event_loop)
                    .iter()
                    .any(|row| row.contains("Invalid pattern")),
                "an invalid regex must be reported; scrollback={:?}",
                scrollback(&event_loop)
            );
            assert_eq!(
                stored_ids(&event_loop).await.0.len(),
                0,
                "an invalid regex must not be stored; scrollback={:?}",
                scrollback(&event_loop)
            );

            // The full wizard: type, tool, pattern, description, test, save.
            event_loop
                .handle_user_input("/patterns add".to_string())
                .await
                .expect("the add command must dispatch");
            answer_dialog(&mut event_loop, DialogResult::Selected(0), "wildcard type").await;
            for (answer, step) in [
                ("bash", "tool name"),
                (pattern_text.as_str(), "pattern"),
                ("offline builds", "description"),
            ] {
                answer_dialog(&mut event_loop, DialogResult::TextEntered(answer.to_string()), step)
                    .await;
            }
            answer_dialog(&mut event_loop, DialogResult::Confirmed(true), "ask to test").await;
            answer_dialog(
                &mut event_loop,
                DialogResult::TextEntered(pattern_text.clone()),
                "test string",
            )
            .await;
            assert!(
                scrollback(&event_loop)
                    .iter()
                    .any(|row| row.contains("Pattern matches the test string.")),
                "the wizard's test step must report the match; scrollback={:?}",
                scrollback(&event_loop)
            );
            assert_eq!(
                stored_ids(&event_loop).await.0.len(),
                0,
                "the wizard must not store the pattern before the owner confirms saving"
            );
            answer_dialog(&mut event_loop, DialogResult::Confirmed(true), "save").await;

            let rows = scrollback(&event_loop);
            let (live, _) = stored_ids(&event_loop).await;
            assert_eq!(
                live.len(),
                1,
                "the completed wizard must add exactly one pattern to the live store; scrollback={rows:?}"
            );
            assert!(
                rows.iter()
                    .any(|row| row.contains(&format!("Pattern saved: {}", &live[0][..8]))),
                "the completed wizard must report the saved pattern's ID, not report itself unimplemented; scrollback={rows:?}"
            );
            assert_eq!(
                ids_on_disk(&store_path).0,
                live,
                "the added pattern must be persisted; scrollback={rows:?}"
            );
            assert_eq!(
                probe_bash_approval(&mut event_loop, command).await,
                ApprovalProbe::AutoApproved,
                "a pattern added by the wizard must be the one the approval path consults; scrollback={rows:?}"
            );
        })
        .await;
}

/// A tool approval that arrives while a `/patterns` confirmation is open
/// replaces it on screen. The owner's answer is then an answer to the tool
/// approval: it must reach the tool, and it must not be taken as consent to
/// clear the standing approvals.
#[tokio::test]
async fn test_patterns_dialog_displaced_by_a_tool_approval_changes_nothing_and_answers_the_tool() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut event_loop, _tempdir, store_path) = patterns_event_loop();
            {
                let mut executor = event_loop.tool_coordinator.tool_executor().lock().await;
                executor.approve_pattern_persistent(pattern_for_bash("cargo build --offline"));
                executor.save_patterns().expect("seed the temporary store");
            }
            let seeded = stored_ids(&event_loop).await;

            event_loop
                .handle_user_input("/patterns clear".to_string())
                .await
                .expect("the clear command must dispatch");
            assert!(
                event_loop.tui_renderer.lock().await.active_dialog.is_some(),
                "clear must open its confirmation dialog; scrollback={:?}",
                scrollback(&event_loop)
            );

            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            event_loop
                .handle_tool_approval_request(
                    Uuid::new_v4(),
                    bash_call("rm notes.txt"),
                    Vec::new(),
                    response_tx,
                )
                .await
                .expect("the tool approval request must be accepted");

            // The owner answers the dialog now on screen: "Yes" to the tool.
            answer_dialog(
                &mut event_loop,
                crate::cli::tui::DialogResult::Selected(0),
                "approve the tool once",
            )
            .await;

            let confirmation =
                tokio::time::timeout(std::time::Duration::from_secs(30), response_rx)
                    .await
                    .expect(
                        "the tool approval hung: its answer was consumed by the displaced \
                         /patterns dialog instead of reaching the tool",
                    )
                    .expect("the tool approval must receive the owner's answer");
            assert!(
                matches!(
                    confirmation,
                    crate::cli::repl_event::events::ConfirmationResult::ApproveOnce
                ),
                "the answer must reach the tool approval it was given to; confirmation={confirmation:?}"
            );
            assert_eq!(
                (stored_ids(&event_loop).await, ids_on_disk(&store_path)),
                (seeded.clone(), seeded),
                "an answer given to a tool approval must never clear the standing approvals; scrollback={:?}",
                scrollback(&event_loop)
            );
        })
        .await;
}

/// Owner-only guard. A peer's prompt reaches this frontend as a named-Brain
/// turn; its text is never parsed as a slash command, so naming a `/patterns`
/// command there cannot list or change the owner's standing approvals.
#[tokio::test]
async fn test_peer_turn_prompt_naming_a_patterns_command_cannot_list_or_change_owner_approvals() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let seeded_pattern = pattern_for_bash("cargo build --offline");
            let prompts = [
                "/patterns".to_string(),
                "/patterns list".to_string(),
                "/patterns clear".to_string(),
                "/patterns add".to_string(),
                format!("/patterns remove {}", seeded_pattern.id),
            ];
            for prompt in prompts {
                let (mut event_loop, _tempdir, store_path) = patterns_event_loop();
                {
                    let mut executor = event_loop.tool_coordinator.tool_executor().lock().await;
                    executor.approve_pattern_persistent(seeded_pattern.clone());
                    executor.save_patterns().expect("seed the temporary store");
                }
                event_loop.runner_brain = Some("home".into());
                event_loop.home_runner_lease_active = true;
                let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
                event_loop
                    .handle_event(super::ReplEvent::NamedBrainTurnRequested(
                        crate::server::RunnerTurnRequest {
                            brain: "home".into(),
                            run_id: crate::brain::RunId(Uuid::new_v4()),
                            request_seq: 1,
                            prompt: prompt.clone(),
                            context: vec![crate::providers::Message::user(prompt.clone())],
                            approval_audience: crate::brain::BrainApprovalAudience {
                                brain_id: crate::brain::BrainId(Uuid::new_v4()),
                                brain: "home".into(),
                                attachment_id: crate::brain::AttachmentId(Uuid::new_v4()),
                                subject: "peer".into(),
                                role: crate::brain::AttachmentRole::Runner,
                                environment_generation: 1,
                            },
                            approval_connection_id: None,
                            grant_ceiling: crate::vm::TypedRuntime::intrinsic_grants(),
                            approval_tx: None,
                            effect_audit: None,
                            response_tx,
                        },
                    ))
                    .await
                    .expect("a named-Brain turn must dispatch");

                let rows = scrollback(&event_loop);
                assert_eq!(
                    (stored_ids(&event_loop).await.0, ids_on_disk(&store_path).0),
                    (vec![seeded_pattern.id.clone()], vec![seeded_pattern.id.clone()]),
                    "a peer's prompt must not change the owner's standing approvals; prompt={prompt:?} scrollback={rows:?}"
                );
                assert!(
                    !rows.iter().any(|row| {
                        row.contains("Tool approval patterns")
                            || row.contains("This will remove")
                            || row.contains("Found pattern to remove")
                            || row.contains("Add Confirmation Pattern")
                            || row.contains(&seeded_pattern.pattern)
                    }),
                    "a peer's prompt must not run a /patterns command or reveal the owner's approvals; prompt={prompt:?} scrollback={rows:?}"
                );
                assert!(
                    event_loop.tui_renderer.lock().await.active_dialog.is_none(),
                    "a peer's prompt must not open a /patterns dialog; prompt={prompt:?}"
                );
            }
        })
        .await;
}

/// `/local` was removed (issue #1633: it sent a one-off query to whichever
/// local model the daemon happened to load, with no history, tools, or model
/// identity). Typed in the live session it must now get the same shape of
/// response as any other unrecognised slash command -- `Command::parse` maps
/// those to `Command::Unknown`, so a short one-line message naming the typed
/// command is shown (issue #1650), never the full help screen. Compared
/// against a slash command that never existed, so the test pins "same shape
/// as unknown" rather than a particular wording.
#[tokio::test]
async fn test_removed_local_command_gets_the_unknown_command_response() {
    async fn scrollback_after(input: &str) -> Vec<String> {
        let mut event_loop = super::EventLoop::new_provider_switch_test_runner(Vec::new(), 0, None);
        event_loop
            .handle_user_input(input.to_string())
            .await
            .expect("an unrecognised slash command must dispatch without error");
        assert!(
            event_loop.conversation.read().await.get_messages().is_empty(),
            "an unrecognised slash command must not be sent to the model as a turn; input={input:?} conversation={:?}",
            event_loop.conversation.read().await.get_messages()
        );
        event_loop
            .output_manager
            .get_messages()
            .iter()
            .map(|message| message.content())
            .collect()
    }

    tokio::task::LocalSet::new()
        .run_until(async {
            let local = scrollback_after("/local hi").await;
            let never_existed = scrollback_after("/no-such-command hi").await;

            assert_eq!(
                local.len(),
                2,
                "the removed /local command must produce its echo and one short response row; scrollback={local:?}"
            );
            assert_eq!(
                local[0], "/local hi",
                "the typed command must be echoed first; scrollback={local:?}"
            );
            assert_eq!(
                local[1], "Unknown command: /local hi. Type /help for the list.",
                "the removed /local command must get the unknown-command message naming itself; scrollback={local:?}"
            );
            assert_eq!(
                never_existed[1], "Unknown command: /no-such-command hi. Type /help for the list.",
                "a never-existing slash command must get the unknown-command message naming itself; scrollback={never_existed:?}"
            );
            assert!(
                !local[1..].iter().any(|message| {
                    message.contains("Local Model Query") || message.contains("not yet implemented")
                }),
                "the response must not come from a local-query handler or the not-implemented catch-all; scrollback={local:?}"
            );
        })
        .await;
}

/// Issue #1650: an unrecognised slash command prints the entire help screen
/// with no indication that the command wasn't recognised. Typed in the live
/// session, `/bogus` must print one short line naming the unrecognised
/// command and pointing at `/help` -- not the full help screen that `/help`
/// itself prints.
#[tokio::test]
async fn test_unknown_command_prints_short_message() {
    async fn scrollback_after(input: &str) -> Vec<String> {
        let mut event_loop = super::EventLoop::new_provider_switch_test_runner(Vec::new(), 0, None);
        event_loop
            .handle_user_input(input.to_string())
            .await
            .expect("an unrecognised slash command must dispatch without error");
        event_loop
            .output_manager
            .get_messages()
            .iter()
            .map(|message| message.content())
            .collect()
    }

    tokio::task::LocalSet::new()
        .run_until(async {
            let bogus = scrollback_after("/bogus").await;
            let help = scrollback_after("/help").await;

            assert_eq!(
                bogus.len(),
                2,
                "an unrecognised slash command must produce its echo and exactly one short response row, not a multi-section help screen; scrollback={bogus:?}"
            );
            assert_eq!(
                bogus[0], "/bogus",
                "the typed command must be echoed first; scrollback={bogus:?}"
            );
            assert_eq!(
                bogus[1], "Unknown command: /bogus. Type /help for the list.",
                "an unrecognised slash command must name itself and point at /help in one short line instead of printing the full help screen; scrollback={bogus:?}"
            );
            let bad_subcommand = scrollback_after("/patterns invalid").await;
            assert_eq!(
                bad_subcommand.get(1).map(String::as_str),
                Some("Unknown command: /patterns invalid. Type /help for the list."),
                "a known command with an unrecognised subcommand must be named in full, so the message does not claim the command itself is unknown; scrollback={bad_subcommand:?}"
            );
            assert_eq!(
                bogus[1].lines().count(),
                1,
                "the unknown-command response must be a single line, not the multi-line help screen; scrollback={bogus:?}"
            );
            assert_ne!(
                bogus[1], help[1],
                "the unknown-command response must differ from the full help screen that genuine /help prints; bogus_scrollback={bogus:?} help_scrollback={help:?}"
            );
        })
        .await;
}

/// A scripted provider whose name is unique to one metrics-isolation test, so
/// a row carrying it can be attributed to that test wherever it lands.
struct MetricsIsolationProvider {
    name: &'static str,
}

#[async_trait::async_trait]
impl crate::generators::Generator for MetricsIsolationProvider {
    async fn generate(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<crate::generators::GeneratorResponse> {
        let text = "(say \"metrics isolation response\")";
        Ok(crate::generators::GeneratorResponse {
            text: text.to_string(),
            content_blocks: vec![crate::providers::ContentBlock::text(text)],
            tool_uses: Vec::new(),
            metadata: crate::generators::ResponseMetadata {
                generator: self.name.to_string(),
                model: self.name.to_string(),
                confidence: None,
                stop_reason: None,
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
                max_context_messages: None,
            };
        &CAPABILITIES
    }

    fn name(&self) -> &str {
        self.name
    }
}

/// Every metrics row under `directory` that names `provider`, as
/// `(file name, line)`. Reads only; a missing directory holds no rows.
fn metrics_rows_naming(directory: &std::path::Path, provider: &str) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let Ok(contents) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let file = entry.file_name().to_string_lossy().into_owned();
        rows.extend(
            contents
                .lines()
                .filter(|line| line.contains(provider))
                .map(|line| (file.clone(), line.to_string())),
        );
    }
    rows
}

/// The directory the event loop used to derive for itself, whatever `HOME`
/// this test process was started with.
fn home_derived_metrics_directory() -> std::path::PathBuf {
    dirs::home_dir()
        .expect("the test process must have a home directory to guard")
        .join(".finch")
        .join("metrics")
}

/// Drive one real turn (`handle_user_input` -> the real `LlmLoop` worker ->
/// `process_query_with_tools`) through a headless runner backed by
/// `provider`, optionally recording into `metrics`.
async fn run_one_metrics_isolation_turn(
    provider: &'static str,
    metrics: Option<Arc<crate::metrics::MetricsLogger>>,
) -> EventLoop {
    let patterns = tempfile::tempdir()
        .expect("isolated metrics-isolation tool state")
        .path()
        .join("patterns.json");
    let executor = crate::tools::ToolExecutor::new(
        crate::tools::ToolRegistry::new(),
        crate::tools::PermissionManager::new(),
        patterns,
    )
    .expect("construct metrics-isolation tool executor");
    let mut event_loop = EventLoop::new_named_brain_test_runner(
        Arc::new(MetricsIsolationProvider { name: provider })
            as Arc<dyn crate::generators::Generator>,
        Vec::new(),
        Arc::new(tokio::sync::Mutex::new(executor)),
        Arc::new(crate::runtime::ProgramRuntime::new()),
    );
    if let Some(metrics) = metrics {
        event_loop.set_metrics_logger_for_test(metrics);
    }
    event_loop.output_manager.disable_stdout();
    event_loop.start_llm_worker();
    event_loop
        .handle_user_input("metrics isolation question".into())
        .await
        .expect("the metrics-isolation turn must dispatch");
    let query_id = event_loop
        .active_query_id
        .read()
        .await
        .expect("the metrics-isolation turn must own the active slot");
    loop {
        let event = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            event_loop.event_rx.recv(),
        )
        .await
        .expect("the metrics-isolation turn hung before its terminal event")
        .expect("the event channel must remain open");
        let complete = matches!(
            event,
            ReplEvent::StreamingComplete { query_id: id, .. } if id == query_id
        );
        event_loop
            .handle_event(event)
            .await
            .expect("metrics-isolation events must dispatch");
        if complete {
            break;
        }
    }
    event_loop
}

/// Issue #1629 (test runs filling the user's `/metrics` report with fixture
/// providers): the headless runner used to build its own logger from
/// `dirs::home_dir()`, so a fixture provider's wire-adherence row landed in
/// `~/.finch/metrics` of whoever ran the suite.
#[tokio::test]
async fn test_headless_runner_fixture_wire_metric_never_reaches_the_home_metrics_directory() {
    const PROVIDER: &str = "metrics-isolation-uninjected-fixture";
    let event_loop = tokio::task::LocalSet::new()
        .run_until(run_one_metrics_isolation_turn(PROVIDER, None))
        .await;

    assert_eq!(
        event_loop.metrics_dir_for_test(),
        None,
        "a headless test runner must hold no metrics logger until a test injects one"
    );
    let home_metrics = home_derived_metrics_directory();
    let leaked = metrics_rows_naming(&home_metrics, PROVIDER);
    assert!(
        leaked.is_empty(),
        "a fixture provider's metrics must never be written under the home directory; \
         directory={} leaked_rows={leaked:?}",
        home_metrics.display()
    );
}

#[tokio::test]
async fn test_injected_metrics_logger_keeps_the_fixture_wire_metric_in_the_tests_own_directory() {
    const PROVIDER: &str = "metrics-isolation-injected-fixture";
    let directory = tempfile::tempdir().expect("the test's own metrics directory");
    let logger = Arc::new(
        crate::metrics::MetricsLogger::new(directory.path().to_path_buf())
            .expect("construct the test's own metrics logger"),
    );
    let event_loop = tokio::task::LocalSet::new()
        .run_until(run_one_metrics_isolation_turn(PROVIDER, Some(logger)))
        .await;

    assert_eq!(
        event_loop.metrics_dir_for_test().as_deref(),
        Some(directory.path()),
        "the event loop must record under exactly the directory the test injected"
    );
    let own = metrics_rows_naming(directory.path(), PROVIDER);
    let wire_rows: Vec<_> = own
        .iter()
        .filter(|(file, _)| file.starts_with("wire-"))
        .collect();
    assert_eq!(
        wire_rows.len(),
        1,
        "one fixture turn must record exactly one wire-adherence row in the test's own \
         directory; directory={} rows={own:?}",
        directory.path().display()
    );
    let home_metrics = home_derived_metrics_directory();
    let leaked = metrics_rows_naming(&home_metrics, PROVIDER);
    assert!(
        leaked.is_empty(),
        "an injected logger must be the only place a fixture provider's metrics land; \
         home_directory={} leaked_rows={leaked:?} own_rows={own:?}",
        home_metrics.display()
    );
}

/// What a [`RequestMetricProvider`] does on each provider call.
enum RequestMetricScript {
    /// Every call answers.
    Succeed,
    /// Every call returns a provider error.
    Fail,
    /// The first call blocks until released; every call then answers.
    BlockFirstCall,
    /// The first call asks for the `late_probe` tool; the second answers.
    ToolRoundThenSucceed,
}

/// A scripted provider for the request-metric tests. Its name is the provider
/// entry (profile) name and its model is distinct, so a row can be checked
/// for both.
struct RequestMetricProvider {
    name: &'static str,
    script: RequestMetricScript,
    calls: std::sync::atomic::AtomicUsize,
    first_call_started: tokio::sync::Notify,
    release_first_call: tokio::sync::Notify,
}

const REQUEST_METRIC_MODEL: &str = "request-metric-model";
const REQUEST_METRIC_PROMPT: &str = "request metric question PROMPT_SENTINEL_1629";
const REQUEST_METRIC_ANSWER: &str = "request metric ANSWER_SENTINEL_1629";

impl RequestMetricProvider {
    fn new(name: &'static str, script: RequestMetricScript) -> Arc<Self> {
        Arc::new(Self {
            name,
            script,
            calls: std::sync::atomic::AtomicUsize::new(0),
            first_call_started: tokio::sync::Notify::new(),
            release_first_call: tokio::sync::Notify::new(),
        })
    }

    fn answer(&self) -> crate::generators::GeneratorResponse {
        let text = format!("(say \"{REQUEST_METRIC_ANSWER}\")");
        crate::generators::GeneratorResponse {
            content_blocks: vec![crate::providers::ContentBlock::text(&text)],
            text,
            tool_uses: Vec::new(),
            metadata: crate::generators::ResponseMetadata {
                generator: self.name.to_string(),
                model: REQUEST_METRIC_MODEL.to_string(),
                confidence: None,
                stop_reason: None,
                input_tokens: None,
                output_tokens: None,
                latency_ms: None,
                primary_allowance_used_percent: None,
                secondary_allowance_used_percent: None,
            },
        }
    }
}

#[async_trait::async_trait]
impl crate::generators::Generator for RequestMetricProvider {
    async fn generate(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<crate::generators::GeneratorResponse> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.script {
            RequestMetricScript::Succeed => Ok(self.answer()),
            RequestMetricScript::Fail => anyhow::bail!("scripted request-metric provider failure"),
            RequestMetricScript::BlockFirstCall => {
                if call == 0 {
                    self.first_call_started.notify_one();
                    self.release_first_call.notified().await;
                }
                Ok(self.answer())
            }
            RequestMetricScript::ToolRoundThenSucceed => {
                if call > 0 {
                    return Ok(self.answer());
                }
                let tool = crate::tools::ToolUse {
                    id: "request-metric-tool".into(),
                    name: "late_probe".into(),
                    input: serde_json::json!({}),
                };
                let mut response = self.answer();
                response
                    .content_blocks
                    .push(crate::providers::ContentBlock::ToolUse {
                        id: tool.id.clone(),
                        name: tool.name.clone(),
                        input: tool.input.clone(),
                    });
                response.tool_uses.push(tool);
                Ok(response)
            }
        }
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
                max_context_messages: None,
            };
        &CAPABILITIES
    }

    fn name(&self) -> &str {
        self.name
    }

    fn model_name(&self) -> &str {
        REQUEST_METRIC_MODEL
    }
}

fn request_metric_cloud_entry(name: &str) -> crate::config::ProviderEntry {
    crate::config::ProviderEntry::Claude {
        api_key: "test-api-key".to_string(),
        model: None,
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some(name.to_string()),
    }
}

fn request_metric_local_entry(name: &str) -> crate::config::ProviderEntry {
    crate::config::ProviderEntry::Local {
        inference_provider: crate::models::InferenceProvider::LlamaCpp,
        execution_target: crate::config::ExecutionTarget::Auto,
        model_family: crate::models::ModelFamily::Gemma2,
        model_size: crate::models::ModelSize::Medium,
        model_path: None,
        managed_artifact: None,
        enabled: true,
        name: Some(name.to_string()),
    }
}

/// A headless event loop on the real worker, backed by `provider`, with
/// `entries` as the session's configured provider entries and a metrics
/// logger over a directory the test owns.
struct RequestMetricSession {
    event_loop: EventLoop,
    metrics: Arc<crate::metrics::MetricsLogger>,
    directory: tempfile::TempDir,
    probe_executions: Arc<std::sync::atomic::AtomicUsize>,
}

impl RequestMetricSession {
    fn start(
        provider: &Arc<RequestMetricProvider>,
        entries: Vec<crate::config::ProviderEntry>,
    ) -> Self {
        let probe_executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register(Box::new(LateProviderProbe {
            executions: Arc::clone(&probe_executions),
        }));
        let definitions = registry.definitions();
        let patterns = tempfile::tempdir()
            .expect("isolated request-metric tool state")
            .path()
            .join("patterns.json");
        let executor = crate::tools::ToolExecutor::new(
            registry,
            crate::tools::PermissionManager::new(),
            patterns,
        )
        .expect("construct request-metric tool executor");
        let mut event_loop = EventLoop::new_test_runner(
            "request-metric-test",
            Arc::clone(provider) as Arc<dyn crate::generators::Generator>,
            definitions,
            Arc::new(tokio::sync::Mutex::new(executor)),
            Arc::new(crate::runtime::ProgramRuntime::new()),
            entries,
            0,
            None,
        );
        let directory = tempfile::tempdir().expect("the test's own metrics directory");
        let metrics = Arc::new(
            crate::metrics::MetricsLogger::new(directory.path().to_path_buf())
                .expect("construct the test's own metrics logger"),
        );
        event_loop.set_metrics_logger_for_test(Arc::clone(&metrics));
        event_loop.output_manager.disable_stdout();
        event_loop.start_llm_worker();
        Self {
            event_loop,
            metrics,
            directory,
            probe_executions,
        }
    }

    /// Submit one user turn through the real input path and return its id.
    async fn submit(&mut self, text: &str) -> uuid::Uuid {
        self.event_loop
            .handle_user_input(text.into())
            .await
            .expect("the request-metric turn must dispatch");
        self.event_loop
            .active_query_id
            .read()
            .await
            .expect("the request-metric turn must own the active slot")
    }

    /// Dispatch events until `query_id` reaches a terminal event.
    async fn pump_until_terminal(&mut self, query_id: uuid::Uuid) {
        loop {
            let event = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                self.event_loop.event_rx.recv(),
            )
            .await
            .expect("the request-metric turn hung before its terminal event")
            .expect("the event channel must remain open");
            let terminal = matches!(
                &event,
                ReplEvent::StreamingComplete { query_id: id, .. }
                    | ReplEvent::QueryFailed { query_id: id, .. } if *id == query_id
            );
            self.event_loop
                .handle_event(event)
                .await
                .expect("request-metric events must dispatch");
            if terminal {
                break;
            }
        }
    }

    /// Every request row in the test's directory, as raw lines.
    fn request_rows(&self) -> Vec<String> {
        let mut rows = Vec::new();
        for entry in std::fs::read_dir(self.directory.path())
            .expect("read the test's own metrics directory")
            .flatten()
        {
            if entry.file_name().to_string_lossy().starts_with("wire-") {
                continue;
            }
            let contents =
                std::fs::read_to_string(entry.path()).expect("read a request metrics file");
            rows.extend(contents.lines().map(str::to_string));
        }
        rows
    }

    fn summary(&self) -> crate::metrics::RequestSummary {
        self.metrics
            .request_summary_last_24_hours()
            .expect("the request summary must be readable")
    }
}

/// The request rows must stay source-free at the production boundary, not
/// only in the type's own serialization test.
fn assert_request_rows_are_source_free(rows: &[String]) {
    for row in rows {
        for sentinel in [
            "PROMPT_SENTINEL_1629",
            "ANSWER_SENTINEL_1629",
            "test-api-key",
        ] {
            assert!(
                !row.contains(sentinel),
                "a request metric must carry no prompt, response, or credential text; \
                 sentinel={sentinel:?} row={row}"
            );
        }
    }
}

/// Issue #1629 ("/metrics request counters are always zero"): nothing under
/// `src/cli/repl_event/` recorded a request, so the live session's `/metrics`
/// block stayed at zero after real turns.
#[tokio::test]
async fn test_completed_turn_records_exactly_one_request_metric_for_its_cloud_provider_entry() {
    const PROVIDER: &str = "request-metric-cloud-entry";
    tokio::task::LocalSet::new()
        .run_until(async {
            let provider = RequestMetricProvider::new(PROVIDER, RequestMetricScript::Succeed);
            let mut session =
                RequestMetricSession::start(&provider, vec![request_metric_cloud_entry(PROVIDER)]);
            assert_eq!(
                session.summary().total,
                0,
                "no request may be recorded before a turn runs; rows={:?}",
                session.request_rows()
            );

            let query_id = session.submit(REQUEST_METRIC_PROMPT).await;
            session.pump_until_terminal(query_id).await;

            let rows = session.request_rows();
            let summary = session.summary();
            assert_eq!(
                rows.len(),
                1,
                "one completed user turn must record exactly one request metric; \
                 rows={rows:?} summary={summary:?}"
            );
            assert_eq!(
                (
                    summary.total,
                    summary.completed,
                    summary.failed,
                    summary.cancelled
                ),
                (1, 1, 0, 0),
                "a completed turn must be counted as completed; rows={rows:?} summary={summary:?}"
            );
            assert_eq!(
                summary.groups,
                vec![crate::metrics::RequestGroupSummary {
                    provider: PROVIDER.into(),
                    model: REQUEST_METRIC_MODEL.into(),
                    kind: Some(crate::metrics::ProviderKind::Cloud),
                    total: 1,
                    completed: 1,
                    failed: 0,
                    cancelled: 0,
                    avg_completed_ms: summary.avg_completed_ms,
                }],
                "the turn must be attributed to its provider entry, model, and the entry's \
                 kind; rows={rows:?}"
            );
            assert!(
                rows[0].contains("\"surface\":\"interactive\""),
                "a turn the session's own user typed is the interactive surface; rows={rows:?}"
            );
            assert_request_rows_are_source_free(&rows);

            let report = crate::cli::commands::format_metrics(&session.metrics)
                .expect("/metrics must render");
            for expected in [
                "  1 request: 1 completed, 0 failed, 0 cancelled",
                "  By provider kind: 1 cloud, 0 local",
                "  request-metric-cloud-entry/request-metric-model (cloud): 1 total, 1 completed",
            ] {
                assert!(
                    report.contains(expected),
                    "/metrics must reflect the turn the event loop just ran; \
                     missing={expected:?} report={report:?}"
                );
            }
        })
        .await;
}

#[tokio::test]
async fn test_turn_on_a_local_provider_entry_is_counted_as_local() {
    const PROVIDER: &str = "request-metric-local-entry";
    tokio::task::LocalSet::new()
        .run_until(async {
            let provider = RequestMetricProvider::new(PROVIDER, RequestMetricScript::Succeed);
            let mut session =
                RequestMetricSession::start(&provider, vec![request_metric_local_entry(PROVIDER)]);
            let query_id = session.submit(REQUEST_METRIC_PROMPT).await;
            session.pump_until_terminal(query_id).await;

            let rows = session.request_rows();
            let summary = session.summary();
            assert_eq!(
                (
                    summary.total,
                    summary.local,
                    summary.cloud,
                    summary.unclassified
                ),
                (1, 1, 0, 0),
                "local versus cloud must be the configured provider entry's kind; \
                 rows={rows:?} summary={summary:?}"
            );
        })
        .await;
}

#[tokio::test]
async fn test_failed_turn_records_exactly_one_failed_request_metric() {
    const PROVIDER: &str = "request-metric-failing-entry";
    tokio::task::LocalSet::new()
        .run_until(async {
            let provider = RequestMetricProvider::new(PROVIDER, RequestMetricScript::Fail);
            let mut session =
                RequestMetricSession::start(&provider, vec![request_metric_cloud_entry(PROVIDER)]);
            let query_id = session.submit(REQUEST_METRIC_PROMPT).await;
            session.pump_until_terminal(query_id).await;

            let rows = session.request_rows();
            let summary = session.summary();
            assert_eq!(
                rows.len(),
                1,
                "one failed user turn must record exactly one request metric; \
                 rows={rows:?} summary={summary:?}"
            );
            assert_eq!(
                (
                    summary.total,
                    summary.completed,
                    summary.failed,
                    summary.cancelled
                ),
                (1, 0, 1, 0),
                "a provider failure must be counted as failed; rows={rows:?} summary={summary:?}"
            );
            assert_eq!(
                summary.avg_completed_ms, None,
                "a failed turn must not contribute a completion time; summary={summary:?}"
            );
            assert_eq!(
                summary.groups[0].provider, PROVIDER,
                "the failure must be attributed to the provider entry; summary={summary:?}"
            );
            assert_request_rows_are_source_free(&rows);
        })
        .await;
}

#[tokio::test]
async fn test_cancelled_turn_records_exactly_one_cancelled_request_metric_despite_late_completion()
{
    const PROVIDER: &str = "request-metric-cancelled-entry";
    tokio::task::LocalSet::new()
        .run_until(async {
            let provider =
                RequestMetricProvider::new(PROVIDER, RequestMetricScript::BlockFirstCall);
            let mut session =
                RequestMetricSession::start(&provider, vec![request_metric_cloud_entry(PROVIDER)]);
            let idle_handles = Arc::strong_count(&provider);

            session.submit(REQUEST_METRIC_PROMPT).await;
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                provider.first_call_started.notified(),
            )
            .await
            .expect("the turn hung before reaching the provider");

            session
                .event_loop
                .handle_event(ReplEvent::CancelQuery)
                .await
                .expect("the user's cancel must dispatch");
            let rows = session.request_rows();
            let summary = session.summary();
            assert_eq!(
                (rows.len(), summary.total, summary.cancelled),
                (1, 1, 1),
                "cancelling a turn must record exactly one cancelled request at the moment \
                 of cancellation; rows={rows:?} summary={summary:?}"
            );

            // Hostile timing: the provider answers after the cancel. Its
            // worker task holds provider handles until it returns, so the
            // handle count falling back to idle is the structural signal
            // that the late completion has fully run.
            provider.release_first_call.notify_one();
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while Arc::strong_count(&provider) > idle_handles {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("the cancelled turn's worker hung after its provider was released");
            while let Ok(event) = session.event_loop.event_rx.try_recv() {
                session
                    .event_loop
                    .handle_event(event)
                    .await
                    .expect("late events of a cancelled turn must dispatch");
            }
            let rows = session.request_rows();
            let summary = session.summary();
            assert_eq!(
                (
                    rows.len(),
                    summary.total,
                    summary.completed,
                    summary.failed,
                    summary.cancelled
                ),
                (1, 1, 0, 0, 1),
                "a provider completing after the cancel must not add or change a request \
                 metric; rows={rows:?} summary={summary:?}"
            );

            // The next turn is its own unit.
            let next = session.submit("request metric follow-up").await;
            session.pump_until_terminal(next).await;
            let rows = session.request_rows();
            let summary = session.summary();
            assert_eq!(
                (
                    rows.len(),
                    summary.total,
                    summary.completed,
                    summary.failed,
                    summary.cancelled
                ),
                (2, 2, 1, 0, 1),
                "the turn after a cancelled one must record its own single request metric; \
                 rows={rows:?} summary={summary:?}"
            );
            assert_request_rows_are_source_free(&rows);
        })
        .await;
}

#[tokio::test]
async fn test_turn_with_a_tool_round_records_one_request_metric_not_one_per_provider_call() {
    const PROVIDER: &str = "request-metric-tool-round-entry";
    tokio::task::LocalSet::new()
        .run_until(async {
            let provider =
                RequestMetricProvider::new(PROVIDER, RequestMetricScript::ToolRoundThenSucceed);
            let mut session =
                RequestMetricSession::start(&provider, vec![request_metric_cloud_entry(PROVIDER)]);
            let query_id = session.submit(REQUEST_METRIC_PROMPT).await;
            session.pump_until_terminal(query_id).await;

            let provider_calls = provider.calls.load(std::sync::atomic::Ordering::SeqCst);
            let tool_runs = session
                .probe_executions
                .load(std::sync::atomic::Ordering::SeqCst);
            let rows = session.request_rows();
            let summary = session.summary();
            assert_eq!(
                (provider_calls, tool_runs),
                (2, 1),
                "the scripted turn must really have made a tool round; rows={rows:?}"
            );
            assert_eq!(
                (rows.len(), summary.total, summary.completed),
                (1, 1, 1),
                "a request is one user turn: its tool rounds must not each record a metric; \
                 provider_calls={provider_calls} tool_runs={tool_runs} rows={rows:?} \
                 summary={summary:?}"
            );
        })
        .await;
}
