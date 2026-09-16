use super::*;
use crate::brain::journal::{
    append_event, BrainEvent, BrainEventKind, BrainId, BRAIN_EVENT_SCHEMA_VERSION,
};

#[test]
fn test_summarize_unhydrated_reads_journal_without_creating_files() {
    let root = tempfile::tempdir().unwrap();
    let name = "shared";
    let directory = root.path().join(name);
    std::fs::create_dir_all(&directory).unwrap();
    let brain_id = BrainId::new();
    append_event(
        Some(root.path()),
        name,
        &BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id,
            seq: 1,
            environment_generation: 1,
            sender: "alice".into(),
            created_ms: 10,
            run_id: None,
            mutation: None,
            kind: BrainEventKind::Prompt {
                text: "hello".into(),
            },
        },
    )
    .unwrap();
    let before: std::collections::BTreeSet<_> = std::fs::read_dir(&directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();

    let summary = summarize_unhydrated(Some(root.path()), name, 0);
    assert_eq!(
        (summary.events, summary.revision, summary.turns),
        (1, 1, 1),
        "unhydrated listing must count committed prompts without folding the reducer; summary={summary:?}"
    );
    let after: std::collections::BTreeSet<_> = std::fs::read_dir(&directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(
        before, after,
        "summarize_unhydrated must not create metadata or audit files; before={before:?} after={after:?}"
    );
}

#[test]
fn test_runner_lease_was_handed_off_requires_matching_completion() {
    let lease_id = RunnerLeaseId(uuid::Uuid::from_u128(1));
    let other = RunnerLeaseId(uuid::Uuid::from_u128(2));
    let handoff_id = crate::brain::run::RunnerHandoffId(uuid::Uuid::from_u128(3));
    let snapshot = BrainSnapshot {
        brain_id: BrainId::nil(),
        name: "shared".into(),
        environment: BrainEnvironment {
            machine: "box".into(),
            workspace: PathBuf::from("."),
            generation: 1,
        },
        revision: 2,
        events: vec![
            BrainEvent {
                schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                brain_id: BrainId::nil(),
                seq: 1,
                environment_generation: 1,
                sender: "alice".into(),
                created_ms: 1,
                run_id: None,
                mutation: None,
                kind: BrainEventKind::RunnerHandoffRequested {
                    handoff: crate::brain::run::BrainRunnerHandoff {
                        handoff_id,
                        from_lease_id: lease_id,
                        requested_by: "alice".into(),
                        target_subject: "bob".into(),
                        environment_generation: 1,
                        requested_ms: 1,
                        expires_ms: 10,
                    },
                },
            },
            BrainEvent {
                schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                brain_id: BrainId::nil(),
                seq: 2,
                environment_generation: 1,
                sender: "bob".into(),
                created_ms: 2,
                run_id: None,
                mutation: None,
                kind: BrainEventKind::RunnerHandoffCompleted {
                    handoff_id,
                    lease: BrainRunnerLease {
                        lease_id: other,
                        subject: "bob".into(),
                        environment_generation: 1,
                        acquired_ms: 2,
                        expires_ms: 20,
                    },
                },
            },
        ],
        program_stack: Vec::new(),
        attachments: Vec::new(),
        runner_lease: None,
        runner_handoff: None,
        runs: Vec::new(),
        tasks: Vec::new(),
        schedules: Vec::new(),
        pending_schedule_dues: Vec::new(),
        effect_audits: Vec::new(),
    };
    assert!(
        snapshot.runner_lease_was_handed_off(lease_id),
        "a completed handoff of this lease must be a terminal frontend fact"
    );
    assert!(
        !snapshot.runner_lease_was_handed_off(other),
        "the successor lease is not itself handed off by this completion"
    );
}
