use super::*;

fn sample_schedule(
    id: u128,
    next_due_ms: u64,
    interval_ms: Option<u64>,
    active: bool,
) -> BrainSchedule {
    BrainSchedule {
        schedule_id: ScheduleId(uuid::Uuid::from_u128(id)),
        initiating_attachment_id: legacy_schedule_attachment_id(),
        created_by: "alice".into(),
        grant_ceiling: crate::vm::EffectSet::pure(),
        language: ProgramLanguage::Lisp,
        source: "(say \"tick\")".into(),
        next_due_ms,
        interval_ms,
        delivery_policy: BrainScheduleDeliveryPolicy::Coalesce,
        module_identity: None,
        active,
    }
}

#[test]
fn test_schedule_due_window_counts_missed_ticks_without_rewriting_policy() {
    let schedule = sample_schedule(1, 1_000, Some(1_000), true);
    let (count, last_due_ms, next_due_ms) = schedule_due_window(&schedule, 3_500).unwrap();
    assert_eq!(
        (count, last_due_ms, next_due_ms),
        (3, 3_000, Some(4_000)),
        "coalesce must count every missed tick and advance to the next future due; \
         count={count} last={last_due_ms} next={next_due_ms:?}"
    );

    let one_shot = sample_schedule(2, 9_000, None, true);
    let one_shot_window = schedule_due_window(&one_shot, 12_000).unwrap();
    assert_eq!(
        one_shot_window,
        (1, 9_000, None),
        "a one-shot must retire after its single due instant; window={one_shot_window:?}"
    );
}

#[test]
fn test_schedule_index_orders_across_brains_and_forgets_on_archive() {
    let mut index = ScheduleIndex::default();
    let early = sample_schedule(1, 10, Some(1_000), true);
    let late = sample_schedule(2, 20, Some(1_000), true);
    let inactive = sample_schedule(3, 5, Some(1_000), false);
    index.upsert("bravo", &late);
    index.upsert("alpha", &early);
    index.upsert("alpha", &inactive);

    assert_eq!(
        index.due_brains(15),
        vec!["alpha".to_string()],
        "selection must name only Brains with work due at or before now, in due order"
    );
    assert_eq!(
        index.due_brains(20),
        vec!["alpha".to_string(), "bravo".to_string()],
        "later due times must not reorder earlier Brains ahead of their due instant"
    );
    assert_eq!(index.next_due_ms(), Some(10));

    index.forget("alpha");
    assert_eq!(
        index.due_brains(20),
        vec!["bravo".to_string()],
        "archive/delete must drop every due slot for that Brain; remaining={:?}",
        index.due_brains(20)
    );
    assert!(
        !index.is_indexed("alpha"),
        "forget must mark the Brain unknown so the next warm can repair it"
    );
}
