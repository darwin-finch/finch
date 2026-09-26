use super::*;
use crate::attachment::AttachmentId;

#[test]
fn test_legacy_prompt_json_deserializes_with_empty_mentions() {
    let kind: BrainEventKind = serde_json::from_str(r#"{"kind":"prompt","text":"hello"}"#).unwrap();
    match kind {
        BrainEventKind::Prompt {
            text,
            attached_mentions,
        } => {
            assert_eq!(text, "hello");
            assert!(
                attached_mentions.is_empty(),
                "legacy Prompt events must load without mention fields: {attached_mentions:?}"
            );
        }
        other => panic!("expected Prompt, got {other:?}"),
    }
}

#[test]
fn test_prompt_attachment_round_trip_keeps_digest_and_content() {
    let kind = BrainEventKind::Prompt {
        text: "explain @src/foo.rs".into(),
        attached_mentions: vec![PromptAttachment {
            path: "src/foo.rs".into(),
            kind: "file".into(),
            sha256: "abc123".into(),
            byte_len: 5,
            truncated: false,
            truncation_note: None,
            content: "hello".into(),
        }],
    };
    let json = serde_json::to_string(&kind).unwrap();
    let loaded: BrainEventKind = serde_json::from_str(&json).unwrap();
    match loaded {
        BrainEventKind::Prompt {
            text,
            attached_mentions,
        } => {
            assert_eq!(text, "explain @src/foo.rs");
            assert_eq!(attached_mentions[0].sha256, "abc123");
            assert_eq!(attached_mentions[0].content, "hello");
        }
        other => panic!("expected Prompt, got {other:?}"),
    }
}

#[test]
fn test_context_compacted_round_trip_keeps_tier_digest_and_provider() {
    let kind = BrainEventKind::ContextCompacted {
        covers_through: 17,
        tier: ContextCompactionTier::Gist,
        digest_or_summary: "top terms: retry, timeout, lease".into(),
        provider: Some("local".into()),
        model: Some("gemma-2-9b".into()),
    };
    let json = serde_json::to_string(&kind).unwrap();
    let loaded: BrainEventKind = serde_json::from_str(&json).unwrap();
    assert_eq!(
        loaded, kind,
        "ContextCompacted must serde round-trip byte-identical; json={json}"
    );
}

#[test]
fn test_context_compacted_without_provider_or_model_round_trips_and_omits_fields() {
    let kind = BrainEventKind::ContextCompacted {
        covers_through: 3,
        tier: ContextCompactionTier::Verbatim,
        digest_or_summary: "unchanged".into(),
        provider: None,
        model: None,
    };
    let json = serde_json::to_string(&kind).unwrap();
    assert!(
        !json.contains("provider") && !json.contains("model"),
        "absent provider/model must be omitted, not serialized as null: {json}"
    );
    let loaded: BrainEventKind = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded, kind);
}

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
        kind: BrainEventKind::Prompt {
            text: text.into(),
            attached_mentions: Vec::new(),
        },
    }
}

#[test]
fn test_context_compacted_event_round_trips_through_real_journal_append_and_read() {
    let root = tempfile::tempdir().unwrap();
    let journal = EventJournal::new(Some(root.path().to_path_buf()));
    let brain_id = BrainId::new();
    let event = BrainEvent {
        schema_version: BRAIN_EVENT_SCHEMA_VERSION,
        brain_id,
        seq: 1,
        environment_generation: 1,
        sender: "daemon".into(),
        created_ms: 1,
        run_id: None,
        mutation: None,
        kind: BrainEventKind::ContextCompacted {
            covers_through: 40,
            tier: ContextCompactionTier::LightlyCompressed,
            digest_or_summary: "sha256:0123abcd".into(),
            provider: Some("local".into()),
            model: Some("gemma-2-9b".into()),
        },
    };
    journal.append("shared", &event).unwrap();

    let reloaded = journal.read("shared").unwrap();
    assert_eq!(
        reloaded.len(),
        1,
        "expected exactly the one appended event, got {reloaded:?}"
    );
    assert_eq!(
        reloaded[0], event,
        "ContextCompacted must replay byte-identical from a real journal file"
    );
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
        kind: BrainEventKind::Prompt {
            text: "go".into(),
            attached_mentions: Vec::new(),
        },
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

#[test]
fn test_legacy_metadata_json_loads_without_selection_fields() {
    let metadata: BrainMetadata = serde_json::from_str(
        r#"{"version":1,"brain_id":"00000000-0000-0000-0000-000000000001","created_ms":1}"#,
    )
    .expect("version-1 metadata without overlay fields must still load");
    assert_eq!(metadata.version, 1);
    assert!(
        metadata.selection.is_empty(),
        "legacy metadata must not invent a provider overlay: {:?}",
        metadata.selection
    );
}

#[test]
fn test_metadata_selection_round_trip_is_secret_free() {
    let metadata = BrainMetadata {
        version: 1,
        brain_id: BrainId(uuid::Uuid::from_u128(2)),
        created_ms: 9,
        selection: BrainProviderSelection {
            provider: Some("chatgpt".into()),
            model: Some("gpt-5.6-sol".into()),
            reasoning_effort: Some("high".into()),
            provider_inherited: false,
        },
    };
    let json = serde_json::to_string(&metadata).unwrap();
    assert!(
        !json.contains("api_key") && !json.contains("sk-"),
        "Brain metadata must never carry secrets: {json}"
    );
    let loaded: BrainMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.selection.provider.as_deref(), Some("chatgpt"));
    assert_eq!(loaded.selection.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(loaded.selection.reasoning_effort.as_deref(), Some("high"));
    assert!(
        !loaded.selection.provider_inherited,
        "explicit overlay must not look inherited: {:?}",
        loaded.selection
    );
}
