use super::*;
use crate::journal::BrainId;

#[test]
fn test_attachment_cursors_round_trip_and_reject_foreign_brain() {
    let root = tempfile::tempdir().unwrap();
    let brain_id = BrainId(uuid::Uuid::new_v4());
    let other_id = BrainId(uuid::Uuid::new_v4());
    let attachment_id = AttachmentId::new();
    let mut attachments = HashMap::new();
    attachments.insert(
        attachment_id,
        BrainAttachment {
            attachment_id,
            subject: "alice".into(),
            role: AttachmentRole::Driver,
            acknowledged_seq: 7,
            connected: false,
            connection_id: None,
        },
    );

    write_cursors(Some(root.path()), "shared", brain_id, &attachments).unwrap();
    let loaded = read_cursors(Some(root.path()), "shared", brain_id).unwrap();
    assert_eq!(
        loaded.get(&attachment_id).copied(),
        Some(7),
        "reconnect must restore the durable acknowledgement cursor; loaded={loaded:?}"
    );

    let mismatch = read_cursors(Some(root.path()), "shared", other_id)
        .expect_err("a swapped Brain identity must not rewind another Brain's cursor");
    assert!(
        mismatch.to_string().contains("identity mismatch"),
        "foreign-brain cursor error must name the mismatch; error={mismatch:#}"
    );
}

#[test]
fn test_sorted_attachments_are_stable_by_id() {
    let left = AttachmentId(uuid::Uuid::from_u128(2));
    let right = AttachmentId(uuid::Uuid::from_u128(1));
    let mut attachments = HashMap::new();
    for (id, subject) in [(left, "b"), (right, "a")] {
        attachments.insert(
            id,
            BrainAttachment {
                attachment_id: id,
                subject: subject.into(),
                role: AttachmentRole::Observer,
                acknowledged_seq: 0,
                connected: false,
                connection_id: None,
            },
        );
    }
    let sorted = sorted_attachments(&attachments);
    assert_eq!(
        sorted
            .iter()
            .map(|attachment| attachment.attachment_id)
            .collect::<Vec<_>>(),
        vec![right, left],
        "snapshot attachment order must be by id, not hash-map iteration; sorted={sorted:?}"
    );
}
