use super::*;

#[test]
fn human_labels_never_leak_debug_enum_names() {
    assert_eq!(
        BrainRunStatus::QueuedForEnvironment.human_label(),
        "Queued — no runner connected"
    );
    assert!(
        !BrainRunStatus::QueuedForEnvironment
            .human_label()
            .to_lowercase()
            .contains("queuedforenvironment"),
        "raw Debug enum names must not reach TUI, raw mode, or screen-reader text"
    );
}

#[test]
fn test_validate_run_transition_rejects_terminal_and_skips() {
    assert!(
        validate_run_transition(BrainRunStatus::Running, BrainRunStatus::Completed).is_ok(),
        "running -> completed is the ordinary success path"
    );
    assert!(
        validate_run_transition(
            BrainRunStatus::QueuedForEnvironment,
            BrainRunStatus::Cancelled
        )
        .is_ok(),
        "queued work may cancel before the environment starts it"
    );
    let late = validate_run_transition(BrainRunStatus::Cancelled, BrainRunStatus::Completed)
        .expect_err("a late completion must not revive a cancelled run");
    assert!(
        late.to_string().contains("invalid Brain run transition")
            || late.to_string().contains("terminal"),
        "late-completion error must name the illegal transition; error={late:#}"
    );
    let skip = validate_run_transition(
        BrainRunStatus::QueuedForEnvironment,
        BrainRunStatus::Completed,
    )
    .expect_err("queued runs cannot skip Running/Cancelled/Failed");
    assert!(
        skip.to_string().contains("invalid Brain run transition"),
        "skipped-step error must name the illegal transition; error={skip:#}"
    );
}

#[test]
fn test_disconnect_intent_survives_restart_and_clears_exactly_once() {
    let root = tempfile::tempdir().unwrap();
    let run_id = RunId::new();
    let intent = DisconnectTerminalizationIntent {
        sender: "alice".into(),
        run_id,
        request_seq: 3,
        status: BrainRunStatus::Interrupted,
        detail: "initiating Brain connection disconnected".into(),
    };
    persist_disconnect_intent(Some(root.path()), "shared", &intent).unwrap();
    let loaded = read_disconnect_intents(Some(root.path()), "shared").unwrap();
    assert_eq!(
        loaded.get(&run_id).map(|item| item.request_seq),
        Some(3),
        "a restarted daemon must recover the durable disconnect intent; loaded={loaded:?}"
    );
    clear_disconnect_intent(Some(root.path()), "shared", run_id).unwrap();
    let empty = read_disconnect_intents(Some(root.path()), "shared").unwrap();
    assert!(
        empty.is_empty(),
        "clearing a published terminalization must remove the intent file; remaining={empty:?}"
    );
}
