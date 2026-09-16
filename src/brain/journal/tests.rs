use super::*;
use crate::brain::attachment::AttachmentId;

fn prompt(brain_id: BrainId, seq: u64, text: &str) -> BrainEvent {
    BrainEvent {
        schema_version: BRAIN_EVENT_SCHEMA_VERSION,
        brain_id,
        seq,
        environment_generation: 1,
        sender: "alice".into(),
        created_ms: seq,
        run_id: None,
        mutation: None,
        kind: BrainEventKind::Prompt { text: text.into() },
    }
}

#[test]
fn test_journal_append_batch_and_torn_tail_replay_keeps_committed_events() {
    let root = tempfile::tempdir().unwrap();
    let journal = EventJournal::new(Some(root.path().to_path_buf()));
    let brain_id = BrainId::new();
    let first = prompt(brain_id, 1, "one");
    let second = prompt(brain_id, 2, "two");
    journal.append("shared", &first).unwrap();
    journal.append_batch("shared", &[second.clone()]).unwrap();

    let path = event_path(Some(root.path()), "shared").unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"seq\":3,\"kind\":\"prompt\"");
    std::fs::write(&path, &bytes).unwrap();

    let scan = scan_readonly(&path);
    assert_eq!(
        scan.events,
        2,
        "readonly scan must ignore a torn tail without truncating; scan={:?} bytes={}",
        (scan.events, scan.revision),
        bytes.len()
    );
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        before, bytes,
        "scan_readonly must never truncate the journal file"
    );

    let loaded = journal.read("shared").unwrap();
    assert_eq!(
        loaded.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![1, 2],
        "restart read must recover only newline-terminated records; loaded={loaded:?}"
    );
    let recovered = std::fs::read(&path).unwrap();
    assert!(
        recovered.ends_with(b"\n") && recovered.len() < before.len(),
        "authoritative read must truncate the torn tail; recovered_len={} before_len={}",
        recovered.len(),
        before.len()
    );
}

#[test]
fn test_replay_mutation_is_idempotent_and_rejects_key_reuse() {
    let attachment_id = AttachmentId(uuid::Uuid::from_u128(1));
    let receipt = BrainMutationReceipt {
        mutation_id: uuid::Uuid::from_u128(9),
        attachment_id,
        expected_revision: 0,
        environment_generation: 1,
        command_sha256: "abc".into(),
    };
    let event = BrainEvent {
        schema_version: BRAIN_EVENT_SCHEMA_VERSION,
        brain_id: BrainId::new(),
        seq: 1,
        environment_generation: 1,
        sender: "alice".into(),
        created_ms: 1,
        run_id: None,
        mutation: Some(receipt.clone()),
        kind: BrainEventKind::Prompt { text: "go".into() },
    };
    let replayed = replay_mutation(std::slice::from_ref(&event), &receipt)
        .unwrap()
        .expect("an identical receipt must replay the original event");
    assert_eq!(replayed.seq, 1);

    let mut reused = receipt.clone();
    reused.command_sha256 = "other".into();
    let error = replay_mutation(std::slice::from_ref(&event), &reused)
        .expect_err("reusing a mutation id with a different command must fail");
    assert!(
        error.to_string().contains("idempotency key"),
        "key-reuse error must name the idempotency invariant; error={error:#}"
    );
    assert!(
        replay_mutation(
            std::slice::from_ref(&event),
            &BrainMutationReceipt {
                mutation_id: uuid::Uuid::from_u128(8),
                ..receipt
            }
        )
        .unwrap()
        .is_none(),
        "an unseen mutation id must not invent a replayed event"
    );
}
