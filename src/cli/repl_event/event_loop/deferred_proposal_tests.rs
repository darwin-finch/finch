use super::*;

fn test_pending_named_brain_turn() -> PendingNamedBrainTurn {
    let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
    PendingNamedBrainTurn {
        brain: "audit-test".into(),
        run_id: crate::brain::store::RunId(uuid::Uuid::new_v4()),
        response_tx,
        turn_events: Vec::new(),
        effect_journal: Vec::new(),
        cancellation_requested: false,
        active_tool_ids: std::collections::HashSet::new(),
        approval_audience: crate::brain::store::BrainApprovalAudience {
            brain_id: crate::brain::store::BrainId(uuid::Uuid::new_v4()),
            brain: "audit-test".into(),
            attachment_id: crate::brain::store::AttachmentId(uuid::Uuid::new_v4()),
            subject: "runner".into(),
            role: crate::brain::store::AttachmentRole::Runner,
            environment_generation: 1,
        },
        approval_tx: None,
        effect_audit: None,
        restart: None,
    }
}

#[test]
fn named_brain_effect_audit_cancellation_quiesces_without_late_tool_result() {
    let mut turn = test_pending_named_brain_turn();
    turn.observe_tool_calls(vec![
        crate::tools::types::ToolUse {
            id: "tool-a".into(),
            name: "submit_program".into(),
            input: serde_json::json!({"source": "secret-a"}),
        },
        crate::tools::types::ToolUse {
            id: "tool-b".into(),
            name: "submit_program".into(),
            input: serde_json::json!({"source": "secret-b"}),
        },
    ]);
    turn.cancellation_requested = true;

    assert_eq!(
        turn.observe_tool_result("tool-a", &Ok("late-a".into())),
        NamedBrainToolResultDisposition::DiscardCancelled { quiesced: false }
    );
    assert_eq!(
        turn.observe_tool_result("tool-b", &Ok("late-b".into())),
        NamedBrainToolResultDisposition::DiscardCancelled { quiesced: true }
    );
    assert!(turn.active_tool_ids.is_empty());
    assert_eq!(
        turn.turn_events
            .iter()
            .filter(|event| matches!(event, crate::server::RunnerTurnEvent::Result { .. }))
            .count(),
        0,
        "cancelled late results must not enter canonical Brain/provider history"
    );
}

#[tokio::test]
async fn extracts_the_exact_suspended_proposal_handle() {
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProgramInvoke,
            selector: crate::vm::ResourceSelector::Program {
                languages: vec!["python".into()],
            },
        })
        .unwrap();
    let outcome = runtime
        .submit_with_deferred_program_effects(
            crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some("proposal-test.lisp".into()),
                source: "(proposal-open \"python\" \"inspect artifact\" \"print('ok')\")".into(),
                intent: "proposal test".into(),
                effect: crate::programs::ExecutionEffect::ExternalWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            },
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
    let proposal =
        deferred_proposal_from_tool_result(&Ok(serde_json::to_string(&outcome).unwrap()))
            .expect("suspended proposal effect");
    assert_eq!(proposal.handle.execution_id, outcome.execution_id);
    assert_eq!(proposal.handle.sequence, 0);
    assert_eq!(proposal.language, "python");
    assert_eq!(proposal.intent, "inspect artifact");
    assert_eq!(proposal.source, "print('ok')");
    let records =
        runner_effect_records_from_tool_result(&Ok(serde_json::to_string(&outcome).unwrap()));
    assert_eq!(records.len(), outcome.effect_journal.len());
    assert!(records.iter().all(|record| {
        record.execution_id == outcome.execution_id
            && outcome.effect_journal.contains(&record.entry)
    }));
}

#[tokio::test]
async fn extracts_the_exact_submit_program_approval_prompt() {
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    let outcome = runtime
        .submit_with_deferred_program_effects(
            crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some("approval-tool-test.lisp".into()),
                source: "(file-read (path \"Cargo.toml\"))".into(),
                intent: "read the manifest".into(),
                effect: crate::programs::ExecutionEffect::WorkspaceRead,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            },
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
    let approval =
        deferred_vm_approval_from_tool_result(&Ok(serde_json::to_string(&outcome).unwrap()))
            .expect("authorization-required tool outcome");

    assert_eq!(approval.prompt.request.execution_id, outcome.execution_id);
    assert_eq!(approval.prompt.request.effect_sequence, Some(0));
    assert_eq!(
        approval.prompt.exact.capability,
        crate::vm::CapabilityKind::FileRead
    );
}

#[tokio::test]
async fn proposal_decision_resumes_the_saved_effect_without_replaying_source() {
    let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProgramInvoke,
            selector: crate::vm::ResourceSelector::Program {
                languages: vec!["python".into()],
            },
        })
        .unwrap();
    let outcome = runtime
        .submit_with_deferred_program_effects(
            crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some("proposal-test.lisp".into()),
                source: "(proposal-open \"python\" \"inspect artifact\" \"print('original')\")"
                    .into(),
                intent: "proposal test".into(),
                effect: crate::programs::ExecutionEffect::ExternalWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            },
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
    let proposal =
        deferred_proposal_from_tool_result(&Ok(serde_json::to_string(&outcome).unwrap()))
            .expect("suspended proposal effect");

    let completed = resume_deferred_proposal(
        runtime.as_ref(),
        &proposal,
        crate::tools::implementations::propose::ProposalDecision::Chat {
            context: "Please explain the artifact first.".into(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        completed.status,
        crate::runtime::outcome::ExecutionStatus::Completed
    );
    assert_eq!(completed.vm_side_effects.len(), 1);
    assert!(matches!(
        completed.values.as_slice(),
        [crate::programs::ProgramValue::Option(Some(value))]
            if matches!(value.as_ref(), crate::programs::ProgramValue::Result { ok: false, value }
                if matches!(value.as_ref(), crate::programs::ProgramValue::String(context)
                    if context == "Please explain the artifact first."))
    ));
    assert!(runtime
        .pending_typed_execution(proposal.handle.execution_id)
        .unwrap()
        .is_none());
}
