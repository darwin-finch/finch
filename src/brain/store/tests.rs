use super::*;
use std::fs::OpenOptions;
use std::io::Write;
// The two shared fixtures live at module scope so the `src/server/handlers.rs`
// boundary tests can use the same ones; aliased back to their local names here.
use super::directory_listing_for_tests as directory_listing;
use super::seed_scheduled_brain_for_tests as seed_scheduled_brain;

// ── #364: the health/status Brain count must not hydrate the store ──────
//
// `list()` calls `load_all`, which `ensure_loaded`s every directory under
// the Brain root: whole event log parsed, three effect-audit SQLite
// databases opened with `synchronous=FULL`, every event folded through the
// reducer, and -- on a directory that lacks them -- `metadata.json`,
// `initialization.json` and those databases *created*. The unauthenticated
// `/health` probe that gates every `finch` launch was calling it to obtain
// a count.

/// A Brain root holding `count` plausible directories, none of them loaded.
///
/// Deliberately shaped like the real thing -- an events log with content --
/// so that a hydrating implementation has real work to skip rather than an
/// empty directory it could shortcut.
fn seed_brain_root(count: usize) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..count {
        let name = format!("brain-{index:04}");
        let directory = temp.path().join(&name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("events.jsonl"),
            "{\"schema_version\":1,\"seq\":1}\n",
        )
        .unwrap();
    }
    temp
}

/// Names resident in the store's in-memory map, i.e. actually hydrated.
fn hydrated_names(store: &BrainStore) -> Vec<String> {
    let mut names: Vec<String> = store
        .brains
        .read()
        .expect("shared brain lock poisoned")
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

#[test]
fn test_count_unhydrated_reports_every_brain_without_loading_one() {
    const BRAINS: usize = 300;
    let temp = seed_brain_root(BRAINS);
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));

    let counted = store.count_unhydrated();

    assert_eq!(
        counted, BRAINS,
        "the unhydrated count must agree with the inventory on disk, or \
             /health reports a Brain population that does not exist; root held \
             {BRAINS} directories and the count was {counted}"
    );
    let hydrated = hydrated_names(&store);
    assert!(
        hydrated.is_empty(),
        "counting Brains must not hydrate any of them -- this is the whole \
             point of #364, since `list()` replays every event log and opens \
             three SQLite databases per Brain on the probe that gates every \
             launch; {} of {BRAINS} were resident after a count, first few: {:?}",
        hydrated.len(),
        hydrated.iter().take(5).collect::<Vec<_>>()
    );
}

#[test]
fn test_list_hydrates_every_brain_which_is_why_the_count_exists() {
    // The negative control for the test above. If this ever stops being
    // true, `list()` has changed and the two can be reunified.
    const BRAINS: usize = 8;
    let temp = seed_brain_root(BRAINS);
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));

    let listed = store.list().expect("list over well-formed Brains");

    assert_eq!(
        listed.len(),
        BRAINS,
        "list must still report every Brain; got {listed:?}"
    );
    assert_eq!(
        hydrated_names(&store).len(),
        BRAINS,
        "list is expected to hydrate -- if it no longer does, #364's \
             separate count is redundant and this test should be deleted along \
             with it; resident after list: {:?}",
        hydrated_names(&store)
    );
}

#[test]
fn test_the_unhydrated_count_agrees_with_list_on_a_healthy_inventory() {
    const BRAINS: usize = 16;
    let temp = seed_brain_root(BRAINS);
    let counting = BrainStore::with_root("box.local", Some(temp.path().into()));
    let listing = BrainStore::with_root("box.local", Some(temp.path().into()));

    let counted = counting.list_names_unhydrated();
    let listed = listing.list().expect("list over well-formed Brains");

    assert_eq!(
        counted, listed,
        "the count must answer the same question as `list`, in the same \
             order, for every Brain that loads -- otherwise /health and \
             /v1/status disagree with every other Brain surface"
    );
}

#[test]
fn test_counting_brains_creates_no_files() {
    // `ensure_loaded` writes: `load_or_create_metadata` and
    // `load_or_create_initialization` create and fsync files, and the
    // effect-audit journals create three SQLite databases. Ordinary
    // startup is supposed to be read-only on user state (#76), and a
    // health probe is the least appropriate place to break that.
    const BRAINS: usize = 12;
    let temp = seed_brain_root(BRAINS);

    let before = walk_paths(temp.path());
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let counted = store.count_unhydrated();
    let after = walk_paths(temp.path());

    assert_eq!(
        counted, BRAINS,
        "the count must be right before its side effects are interesting; \
             root held {BRAINS} directories and the count was {counted}"
    );
    assert_eq!(
        before,
        after,
        "counting Brains must not create, remove or rename anything under \
             the Brain root; appeared: {:?}, disappeared: {:?}",
        after.difference(&before).collect::<Vec<_>>(),
        before.difference(&after).collect::<Vec<_>>()
    );
}

#[test]
fn test_list_summaries_unhydrated_does_not_hydrate_or_create_files() {
    const BRAINS: usize = 8;
    let temp = seed_brain_root(BRAINS);
    let before = walk_paths(temp.path());
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let summaries = store.list_summaries_unhydrated();
    let after = walk_paths(temp.path());

    assert_eq!(
        summaries.len(),
        BRAINS,
        "summaries must cover the same inventory as list_names_unhydrated"
    );
    assert!(
        hydrated_names(&store).is_empty(),
        "summaries must not hydrate; resident: {:?}",
        hydrated_names(&store)
    );
    assert_eq!(
        before,
        after,
        "summaries must not create, remove or rename anything under the Brain root; \
         appeared: {:?}, disappeared: {:?}",
        after.difference(&before).collect::<Vec<_>>(),
        before.difference(&after).collect::<Vec<_>>()
    );
}

#[test]
fn test_list_summaries_unhydrated_reports_turns_attachments_agents_and_size() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let writer = BrainStore::with_root("box.local", Some(root.clone()));
    writer
        .push(
            "busy",
            "alice",
            BrainEventKind::Prompt {
                text: "hello".into(),
            },
        )
        .unwrap();
    writer
        .push(
            "busy",
            "alice",
            BrainEventKind::Prompt {
                text: "again".into(),
            },
        )
        .unwrap();
    let prompt_seq = writer.snapshot("busy").unwrap().revision;
    let attachment = writer
        .attach("busy", "alice@box.local", AttachmentRole::Driver, None)
        .unwrap();
    writer
        .activate_connection(
            "busy",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    let run = writer
        .start_run(
            "busy",
            "alice@box.local",
            BrainRunKind::Subagent,
            prompt_seq,
            attachment.attachment_id,
            BrainRunStatus::Running,
        )
        .unwrap();
    drop(writer);

    let before = walk_paths(&root);
    let reader = BrainStore::with_root("box.local", Some(root.clone()));
    let summaries = reader.list_summaries_unhydrated();
    let after = walk_paths(&root);

    assert_eq!(
        summaries.len(),
        1,
        "exactly the written Brain must appear: {summaries:?}"
    );
    let summary = &summaries[0];
    assert_eq!(summary.name, "busy");
    assert_eq!(
        summary.turns, 2,
        "each Prompt event is one turn; summary={summary:?}"
    );
    assert!(
        summary.bytes > 0,
        "event log and metadata must contribute to size; summary={summary:?}"
    );
    assert_eq!(
        summary.attached,
        vec![BrainListAttachment {
            subject: "alice@box.local".into(),
            role: AttachmentRole::Driver,
        }],
        "activated driver must be listed as attached; summary={summary:?}"
    );
    assert_eq!(
        summary.agents.len(),
        1,
        "running Subagent must appear as a live agent; summary={summary:?}"
    );
    assert_eq!(summary.agents[0].run_id, run.run_id);
    assert_eq!(summary.agents[0].status, BrainRunStatus::Running);
    assert_eq!(summary.agents[0].initiated_by, "alice@box.local");
    assert!(
        hydrated_names(&reader).is_empty(),
        "a fresh store must remain unhydrated after summaries; resident: {:?}",
        hydrated_names(&reader)
    );
    assert_eq!(
        before,
        after,
        "summaries must not mutate the written Brain tree; appeared: {:?}, disappeared: {:?}",
        after.difference(&before).collect::<Vec<_>>(),
        before.difference(&after).collect::<Vec<_>>()
    );
}

fn walk_paths(root: &std::path::Path) -> std::collections::BTreeSet<PathBuf> {
    let mut found = std::collections::BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(path.clone());
            }
            found.insert(path);
        }
    }
    found
}

#[test]
fn test_the_count_survives_a_hostile_brain_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Well-formed.
    for name in ["alpha", "beta_two", "gamma-3"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("events.jsonl"), "{}\n").unwrap();
    }
    // A name `validate_name` rejects: `load_all` skips it, so must the count.
    std::fs::create_dir_all(root.join("has spaces and !")).unwrap();
    std::fs::create_dir_all(
        root.join("sixty-five-characters-is-one-past-the-limit-aaaaaaaaaaaaaaaaaaaaaaa"),
    )
    .unwrap();
    // A regular file, not a directory: `load_all` skips it, so must the count.
    std::fs::write(root.join("loose-file.json"), "{}").unwrap();
    // Interrupted writes: a Brain whose event log is a torn tail, and one
    // that has a directory and nothing else.
    std::fs::create_dir_all(root.join("torn")).unwrap();
    std::fs::write(root.join("torn").join("events.jsonl"), "{\"seq\":1}\n{\"se").unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    // A directory whose metadata is unparseable JSON. `list()` errors on
    // this; the count must not.
    std::fs::create_dir_all(root.join("corrupt")).unwrap();
    std::fs::write(root.join("corrupt").join("metadata.json"), "{not json").unwrap();

    let store = BrainStore::with_root("box.local", Some(root.into()));
    let names = store.list_names_unhydrated();

    assert_eq!(
        names,
        vec![
            "alpha".to_string(),
            "beta_two".to_string(),
            "corrupt".to_string(),
            "empty".to_string(),
            "gamma-3".to_string(),
            "torn".to_string(),
        ],
        "the count applies exactly `load_all`'s rules -- directories only, \
             `validate_name` only -- and reports a Brain whose contents are \
             torn or corrupt rather than failing or omitting it, because a \
             health probe answering 'how many Brains' should not be the thing \
             that goes down when one of them is unreadable (#344)"
    );
    assert!(
        hydrated_names(&store).is_empty(),
        "and it still hydrated nothing; resident: {:?}",
        hydrated_names(&store)
    );
    assert!(
        store.list().is_err(),
        "negative control: `list()` genuinely fails on this root, which is \
             the behaviour #364 removes from the health probe. If list starts \
             tolerating a corrupt Brain, this test is asserting a difference \
             that no longer exists"
    );
}

#[test]
fn test_an_empty_brain_root_counts_zero() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(
        store.count_unhydrated(),
        0,
        "an empty root is zero Brains, not an error; names were {:?}",
        store.list_names_unhydrated()
    );
}

#[test]
fn test_a_missing_brain_root_counts_zero() {
    let temp = tempfile::tempdir().unwrap();
    let absent = temp.path().join("never-created");
    let store = BrainStore::with_root("box.local", Some(absent));
    assert_eq!(
        store.count_unhydrated(),
        0,
        "a root that does not exist yet is zero Brains -- `load_all` \
             returns Ok on an unreadable root and the count must agree"
    );
}

#[test]
fn test_counting_then_loading_still_hydrates_correctly_and_once() {
    // Counting must not poison the loader: a Brain counted and then used
    // has to hydrate normally, exactly once.
    let temp = tempfile::tempdir().unwrap();
    let seeding = BrainStore::with_root("box.local", Some(temp.path().into()));
    seeding.seed_history_for_test("shared", 6).unwrap();
    drop(seeding);

    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(store.count_unhydrated(), 1);
    assert_eq!(store.resident_brain_count(), 0);

    let first = store.snapshot("shared").unwrap();
    let resident_after_first = store.resident_brain_count();
    let second = store.snapshot("shared").unwrap();
    let resident_after_second = store.resident_brain_count();

    assert_eq!(
        first.revision, 6,
        "a Brain that was counted must still replay its whole journal when \
             something actually needs it; got revision {}",
        first.revision
    );
    assert_eq!(
        second.revision, first.revision,
        "and a second read must agree with the first"
    );
    assert_eq!(
        resident_after_first, 1,
        "the first use must hydrate the Brain the count skipped"
    );
    assert_eq!(
        resident_after_second, resident_after_first,
        "and the second use must not hydrate it again -- `ensure_loaded` \
             early-returns on the resident map, which is what makes hydration \
             exactly-once per store"
    );

    // `resident_brain_count` is per-store, so this holds under libtest's
    // concurrency. A process-global hydration counter would not: several
    // tests in this file hydrate, and any before/after delta on a global
    // would be a race.
}

#[test]
fn test_counting_survives_a_brain_root_mutating_underneath_it() {
    // `list_names_unhydrated` takes the resident-map read lock, releases
    // it, then `read_dir`s. A Brain appearing or being hydrated between
    // the two must not panic, double-count, or lose an existing name.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    for index in 0..40 {
        std::fs::create_dir_all(root.join(format!("stable-{index:04}"))).unwrap();
    }
    let store = std::sync::Arc::new(BrainStore::with_root("box.local", Some(root.clone())));

    let churn_root = root.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let churn_stop = std::sync::Arc::clone(&stop);
    let churn = std::thread::spawn(move || {
        let mut index = 0u32;
        while !churn_stop.load(std::sync::atomic::Ordering::Relaxed) {
            let path = churn_root.join(format!("churn-{index:04}"));
            let _ = std::fs::create_dir_all(&path);
            let _ = std::fs::remove_dir_all(&path);
            index = index.wrapping_add(1);
        }
    });

    for _ in 0..200 {
        let names = store.list_names_unhydrated();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(
            names, sorted,
            "the count must stay sorted while the root changes underneath \
                 it, because callers compare it against `list()`; got {names:?}"
        );
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(
            names.len(),
            deduped.len(),
            "and must never report a name twice; got {names:?}"
        );
        for index in 0..40 {
            let stable = format!("stable-{index:04}");
            assert!(
                names.contains(&stable),
                "a Brain that never moved must be reported on every pass; \
                     {stable} was missing from {names:?}"
            );
        }
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    churn.join().unwrap();
}

#[test]
fn test_the_count_is_stable_across_a_store_restart() {
    let temp = tempfile::tempdir().unwrap();
    let seeding = BrainStore::with_root("box.local", Some(temp.path().into()));
    for index in 0..9 {
        seeding
            .seed_history_for_test(&format!("brain-{index:04}"), 3)
            .unwrap();
    }
    drop(seeding);

    let first = BrainStore::with_root("box.local", Some(temp.path().into()));
    let before_restart = first.list_names_unhydrated();
    drop(first);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let after_restart = restarted.list_names_unhydrated();

    assert_eq!(
        before_restart, after_restart,
        "the Brain count is a fact about the filesystem, so a daemon \
             restart must not change it -- if it does, /health's answer depends \
             on daemon uptime"
    );
    assert_eq!(
        restarted.resident_brain_count(),
        0,
        "and a restarted store must still not hydrate to answer it"
    );
}

/// Not a gate -- a measurement, printed with `--nocapture`, so the #364
/// change can be reported with a number instead of an adjective.
/// `#[ignore]` so it never runs in CI and never fails a build.
///
/// Set `FINCH_BENCH_BRAIN_ROOT` to measure a real inventory instead of a
/// synthetic one. Point it at a *copy*: `list()` writes.
#[test]
#[ignore = "measurement, not a gate: run with --ignored --nocapture"]
fn bench_list_versus_count_over_a_realistic_brain_root() {
    const BRAINS: usize = 113;
    const EVENTS: u64 = 20;

    let synthetic;
    let (root, described) = match std::env::var_os("FINCH_BENCH_BRAIN_ROOT") {
        Some(path) => (
            PathBuf::from(path),
            "the inventory named by FINCH_BENCH_BRAIN_ROOT",
        ),
        None => {
            // Build the store through its own API so every Brain is
            // genuinely well-formed: correct identity, a real journal, and
            // the effect-audit databases `ensure_loaded` expects. Seeding
            // hand-written JSON measures the parse-failure path instead.
            synthetic = tempfile::tempdir().unwrap();
            let seeding = BrainStore::with_root("box.local", Some(synthetic.path().into()));
            for index in 0..BRAINS {
                let name = format!("brain-{index:04}");
                let brain_id = seeding.snapshot(&name).unwrap().brain_id;
                for seq in 1..=EVENTS {
                    seeding
                        .append_event(&name, &journal_event(brain_id, seq, "seed"))
                        .unwrap();
                }
            }
            drop(seeding);
            (synthetic.path().to_path_buf(), "a synthetic inventory")
        }
    };

    let directories = std::fs::read_dir(&root)
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    let bytes: u64 = walk_paths(&root)
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .sum();

    let counting = BrainStore::with_root("box.local", Some(root.clone()));
    let count_start = std::time::Instant::now();
    let counted = counting.count_unhydrated();
    let count_elapsed = count_start.elapsed();

    let listing = BrainStore::with_root("box.local", Some(root));
    let list_start = std::time::Instant::now();
    let listed = listing.list();
    let list_elapsed = list_start.elapsed();
    let hydrated = hydrated_names(&listing).len();

    println!(
        "\n#364 -- what GET /health costs over {described}\n  \
             inventory:        {directories} directories, {bytes} bytes on disk\n  \
             count_unhydrated: {counted} Brains in {count_elapsed:?} (0 hydrated)\n  \
             list (hydrating): {} in {list_elapsed:?} ({hydrated} hydrated)\n",
        match &listed {
            Ok(names) => format!("{} Brains", names.len()),
            Err(error) => format!("FAILED: {error}"),
        }
    );
}

#[test]
fn test_a_rootless_store_counts_its_resident_brains() {
    let store = BrainStore::with_root("box.local", None);
    store.snapshot("in-memory").unwrap();
    assert_eq!(
        store.list_names_unhydrated(),
        vec!["in-memory".to_string()],
        "an in-memory store has no directory to scan, so the count must \
             fall back to what is resident, exactly as `list` does"
    );
}

fn journal_event(brain_id: BrainId, seq: u64, text: &str) -> BrainEvent {
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
fn journal_reads_legacy_and_batch_records_with_logical_cursors() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let id = store.snapshot("shared").unwrap().brain_id;
    store
        .append_event("shared", &journal_event(id, 1, "legacy"))
        .unwrap();
    store
        .append_event_batch(
            "shared",
            &[
                journal_event(id, 2, "first"),
                journal_event(id, 3, "second"),
            ],
        )
        .unwrap();
    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(restarted.snapshot("shared").unwrap().revision, 3);
}

#[test]
fn restart_ignores_torn_batch_tail() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let id = store.snapshot("shared").unwrap().brain_id;
    store
        .append_event("shared", &journal_event(id, 1, "committed"))
        .unwrap();
    let encoded = serde_json::to_vec(&BrainJournalRecord::EventBatch {
        event_count: None,
        payload_sha256: None,
        events: vec![
            journal_event(id, 2, "hidden"),
            journal_event(id, 3, "hidden-too"),
        ],
    })
    .unwrap();
    OpenOptions::new()
        .append(true)
        .open(temp.path().join("shared/events.jsonl"))
        .unwrap()
        .write_all(&encoded[..encoded.len() / 2])
        .unwrap();
    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(restarted.snapshot("shared").unwrap().revision, 1);
    restarted
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "after recovery".into(),
            },
        )
        .unwrap();
    drop(restarted);
    let restarted_again = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(restarted_again.snapshot("shared").unwrap().revision, 2);
}

fn mutation_receipt(
    store: &BrainStore,
    attachment_id: AttachmentId,
    mutation_id: uuid::Uuid,
    expected_revision: u64,
    fingerprint: &str,
) -> BrainMutationReceipt {
    BrainMutationReceipt {
        mutation_id,
        attachment_id,
        expected_revision,
        environment_generation: store.environment().generation,
        command_sha256: fingerprint.into(),
    }
}

#[test]
fn mutation_receipt_replays_from_the_canonical_log_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let mutation_id = uuid::Uuid::new_v4();
    let receipt = mutation_receipt(
        &store,
        attachment.attachment_id,
        mutation_id,
        0,
        "sha256:first",
    );
    let first = store
        .push_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "once".into(),
            },
            receipt.clone(),
        )
        .unwrap();
    assert!(!first.replayed);
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let replayed = restarted
        .push_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "once".into(),
            },
            receipt,
        )
        .unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.event.seq, first.event.seq);
    assert_eq!(restarted.snapshot("shared").unwrap().revision, 1);
}

#[test]
fn executable_mutation_and_run_replay_from_one_record_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = AttachmentId::new();
    let receipt = mutation_receipt(&store, attachment, uuid::Uuid::new_v4(), 0, "prompt");
    let first = store
        .push_executable_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "once".into(),
            },
            receipt.clone(),
            attachment,
            BrainRunStatus::QueuedForEnvironment,
        )
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(temp.path().join("shared/events.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let replay = restarted
        .push_executable_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "once".into(),
            },
            receipt,
            attachment,
            BrainRunStatus::Running,
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.accepted, first.accepted);
    assert_eq!(replay.run, first.run);
    assert_eq!(restarted.snapshot("shared").unwrap().revision, 2);
}

#[test]
fn mutation_receipt_rejects_stale_revision_and_fingerprint_reuse() {
    let store = BrainStore::with_root("box.local", None);
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let mutation_id = uuid::Uuid::new_v4();
    store
        .push_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "once".into(),
            },
            mutation_receipt(
                &store,
                attachment.attachment_id,
                mutation_id,
                0,
                "sha256:first",
            ),
        )
        .unwrap();

    let conflict = store.push_idempotent(
        "shared",
        "alice",
        BrainEventKind::Prompt {
            text: "forged".into(),
        },
        mutation_receipt(
            &store,
            attachment.attachment_id,
            mutation_id,
            0,
            "sha256:different",
        ),
    );
    assert!(conflict
        .unwrap_err()
        .to_string()
        .contains("reused with a different command"));

    let changed_revision = store.push_idempotent(
        "shared",
        "alice",
        BrainEventKind::Prompt {
            text: "once".into(),
        },
        mutation_receipt(
            &store,
            attachment.attachment_id,
            mutation_id,
            99,
            "sha256:first",
        ),
    );
    assert!(changed_revision
        .unwrap_err()
        .to_string()
        .contains("different command or precondition"));
    let mut changed_environment = mutation_receipt(
        &store,
        attachment.attachment_id,
        mutation_id,
        0,
        "sha256:first",
    );
    changed_environment.environment_generation += 1;
    assert!(store
        .push_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "once".into()
            },
            changed_environment,
        )
        .unwrap_err()
        .to_string()
        .contains("different command or precondition"));

    let stale = store.push_idempotent(
        "shared",
        "alice",
        BrainEventKind::Prompt {
            text: "stale".into(),
        },
        mutation_receipt(
            &store,
            attachment.attachment_id,
            uuid::Uuid::new_v4(),
            0,
            "sha256:stale",
        ),
    );
    assert!(stale
        .unwrap_err()
        .to_string()
        .contains("expected revision 0 but current revision is 1"));
    assert_eq!(store.snapshot("shared").unwrap().revision, 1);
}

#[test]
fn schedule_creation_replays_the_original_identity_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let receipt = mutation_receipt(
        &store,
        attachment.attachment_id,
        uuid::Uuid::new_v4(),
        0,
        "sha256:schedule",
    );
    let created = store
        .create_schedule_with_receipt(
            "shared",
            "alice",
            attachment.attachment_id,
            ProgramLanguage::Lisp,
            "(say \"once\")".into(),
            crate::vm::EffectSet::pure(),
            1_000,
            None,
            BrainScheduleDeliveryPolicy::Coalesce,
            Some(receipt.clone()),
        )
        .unwrap();
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    restarted
        .attach(
            "shared",
            "alice",
            AttachmentRole::Driver,
            Some(attachment.attachment_id),
        )
        .unwrap();
    let replayed = restarted
        .create_schedule_with_receipt(
            "shared",
            "alice",
            attachment.attachment_id,
            ProgramLanguage::Lisp,
            "(say \"once\")".into(),
            crate::vm::EffectSet::pure(),
            1_000,
            None,
            BrainScheduleDeliveryPolicy::Coalesce,
            Some(receipt),
        )
        .unwrap();
    assert_eq!(replayed.schedule_id, created.schedule_id);
    assert_eq!(restarted.snapshot("shared").unwrap().schedules.len(), 1);
}

// ── #374: due-ordered selection instead of per-Brain enumeration ────────

#[test]
fn test_due_selection_names_only_brains_with_work() {
    // The property the index exists for. Selecting by Brain meant
    // enumerating and replaying the whole store once a second to discover
    // that nothing was due.
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    seed_scheduled_brain(&store, "due-now", 1_000);
    seed_scheduled_brain(&store, "due-later", 9_000_000);

    let due = store.due_schedule_brains(2_000);

    assert_eq!(
        due,
        vec!["due-now".to_string()],
        "selection must name only Brains with schedules due at or before \
             the given instant; a Brain due far in the future must not be \
             woken, or the daemon is back to hydrating the whole store"
    );
}

#[test]
fn test_due_selection_orders_across_brains_by_due_time_not_by_name() {
    // Ordering has to be global. Per-Brain sorting -- which is what
    // existed -- cannot express "this Brain's 10:00 comes before that
    // Brain's 09:00".
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    seed_scheduled_brain(&store, "zulu", 1_000);
    seed_scheduled_brain(&store, "alpha", 5_000);

    let due = store.due_schedule_brains(10_000);

    assert_eq!(
        due,
        vec!["zulu".to_string(), "alpha".to_string()],
        "due order must follow next_due_ms across Brains, not Brain name: \
             'zulu' is due at 1000 and 'alpha' at 5000, so a name-ordered or \
             enumeration-ordered result would put them the other way round"
    );
}

#[test]
fn test_a_schedule_due_exactly_now_is_selected() {
    // An exclusive upper bound would skip a schedule landing precisely on
    // the instant the loop woke for, deferring it a whole cycle.
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    seed_scheduled_brain(&store, "boundary", 4_000);

    assert_eq!(
        store.due_schedule_brains(4_000),
        vec!["boundary".to_string()],
        "a schedule due at exactly the selection instant must be included"
    );
    assert!(
        store.due_schedule_brains(3_999).is_empty(),
        "and one due a millisecond later must not be"
    );
}

#[test]
fn test_the_index_head_is_the_earliest_schedule_in_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(
        store.next_schedule_due_ms(),
        None,
        "an empty store has no head, which is what lets the delivery loop \
             wait instead of polling"
    );

    seed_scheduled_brain(&store, "later", 8_000);
    assert_eq!(store.next_schedule_due_ms(), Some(8_000));

    seed_scheduled_brain(&store, "sooner", 2_000);
    assert_eq!(
        store.next_schedule_due_ms(),
        Some(2_000),
        "a schedule created after the loop began sleeping must become the \
             head, or it fires late by the length of the previous sleep"
    );
}

#[test]
fn test_cancelling_a_schedule_removes_it_from_the_index() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let (attachment_id, schedule_id) = seed_scheduled_brain(&store, "shared", 1_000);
    assert_eq!(store.indexed_schedule_count(), 1);

    assert!(store
        .cancel_schedule("shared", "alice", attachment_id, schedule_id)
        .unwrap());

    assert_eq!(
        store.indexed_schedule_count(),
        0,
        "a cancelled schedule must leave the index, or the delivery loop \
             keeps waking for work that will never run"
    );
    assert_eq!(store.next_schedule_due_ms(), None);
    assert!(store.due_schedule_brains(u64::MAX).is_empty());
}

#[test]
fn test_firing_a_schedule_advances_its_index_position() {
    // Remove-and-reinsert on fire. If the index kept the old key the loop
    // would re-select the same schedule forever.
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    seed_scheduled_brain(&store, "shared", 1_000);
    assert_eq!(store.next_schedule_due_ms(), Some(1_000));

    let queued = store.queue_due_schedules("shared", 1_500).unwrap();
    assert_eq!(queued.len(), 1, "the schedule was due and must have queued");

    assert_eq!(
        store.next_schedule_due_ms(),
        Some(2_000),
        "firing must advance the indexed position by the interval, or the \
             same occurrence is selected again on the next pass; the schedule \
             was due at 1000 with a 1000 ms interval"
    );
    assert_eq!(
        store.indexed_schedule_count(),
        1,
        "and advancing must replace the entry rather than add a second one"
    );
}

#[test]
fn test_the_index_survives_a_store_restart_via_warm_up() {
    // Schedules are only known once a Brain is loaded, so a fresh daemon
    // starts with an empty index and must warm it once.
    let temp = tempfile::tempdir().unwrap();
    {
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&store, "persisted", 3_000);
    }

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(
        restarted.next_schedule_due_ms(),
        None,
        "a restarted store has hydrated nothing yet, so its index is empty \
             -- this is the state that makes the warm-up necessary"
    );

    restarted.warm_schedule_index();

    assert_eq!(
        restarted.next_schedule_due_ms(),
        Some(3_000),
        "after warming, the persisted schedule is selectable again"
    );
    assert_eq!(
        restarted.due_schedule_brains(3_000),
        vec!["persisted".to_string()]
    );
}

#[test]
fn test_warm_up_skips_a_brain_it_cannot_replay_and_keeps_the_rest() {
    // One unreadable Brain must not leave every other Brain unscheduled.
    // The naming, diagnostics and repair-retry semantics are #371's; what
    // is asserted here is only that the index still gets built.
    let temp = tempfile::tempdir().unwrap();
    {
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&store, "healthy-one", 1_000);
        seed_scheduled_brain(&store, "healthy-two", 2_000);
    }
    let corrupt = temp.path().join("corrupt");
    std::fs::create_dir_all(&corrupt).unwrap();
    std::fs::write(corrupt.join("metadata.json"), "{not json").unwrap();

    // The negative control runs on its own store. Sharing one with the
    // subject was wrong: `list()` -> `load_all` -> `ensure_loaded` populates
    // the index from its tail for every Brain it manages to load before it
    // bails, so on a filesystem that yields `corrupt` last the control did
    // the work under test and the assertion below would hold even with
    // `warm_schedule_index`'s body replaced by `Ok(())`. `read_dir` order is
    // unspecified, so that made the test's meaning depend on the filesystem.
    let control = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert!(
        control.list().is_err(),
        "negative control: this root genuinely defeats the enumerating \
             path, which is what made one bad Brain fatal to delivery"
    );
    drop(control);

    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(
        store.indexed_schedule_count(),
        0,
        "the subject store must start empty, so what the assertion below \
             observes is the warm-up's work and nothing else"
    );
    store.warm_schedule_index();

    assert_eq!(
        store.due_schedule_brains(5_000),
        vec!["healthy-one".to_string(), "healthy-two".to_string()],
        "both healthy Brains must still be scheduled, in due order, with \
             the unreplayable one simply absent"
    );
}

#[test]
fn test_archiving_a_scheduled_brain_does_not_resurrect_it() {
    // Review round 1, finding 1. The index kept pointing at an archived
    // Brain, so the delivery loop selected it and `queue_due_schedules` ->
    // `ensure_loaded` -> `load_or_create_metadata` recreated the directory
    // with a *new* BrainId. Archiving a Brain that had an active schedule
    // silently resurrected it as an empty one, on disk.
    //
    // The `forget_schedules_locked` call originally added went on
    // `remove_if_unused`, which refuses to remove any Brain whose history
    // contains a schedule event -- so the cleanup was installed where it
    // could never have entries and missing where it always does.
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    seed_scheduled_brain(&store, "doomed", 1_000);
    assert_eq!(
        store.indexed_schedule_count(),
        1,
        "precondition: the Brain has an indexed active schedule"
    );

    store.archive("doomed").unwrap();

    assert_eq!(
        store.indexed_schedule_count(),
        0,
        "archiving must drop the Brain's index entries; a stale entry makes \
             the delivery loop select a Brain that no longer exists"
    );
    assert_eq!(
        store.next_schedule_due_ms(),
        None,
        "and the archived schedule must not remain the index head"
    );
    assert!(
        store.due_schedule_brains(u64::MAX).is_empty(),
        "selection must not name an archived Brain at any instant; naming \
             it is what drives the recreation"
    );
    assert!(
        !temp.path().join("doomed").exists(),
        "the archived Brain directory must stay gone -- if the index still \
             named it, selecting it would recreate it here with a fresh BrainId"
    );
}

// ── #383: a Brain that left by a route the daemon did not perform ───────

/// Everything the due index currently holds, as `(due_ms, brain, schedule)`
/// triples, for assertion diagnostics. Reads the index, never rebuilds it.
fn indexed_due_keys(store: &BrainStore) -> Vec<(u64, String, ScheduleId)> {
    store
        .schedule_index
        .read()
        .expect("schedule index lock poisoned")
        .due_keys()
}

/// Drop the in-memory copy of a Brain without touching its disk state or
/// its index entries, asserting it was resident first. See
/// `BrainStore::evict_resident_brain_for_tests` for why this state matters.
fn evict_resident_brain(store: &BrainStore, name: &str) {
    let evicted = store.evict_resident_brain_for_tests(name);
    assert!(
        evicted,
        "test setup: '{name}' was expected to be resident before eviction, \
             so that what follows exercises the unhydrated delivery path; it \
             was not, which would make the test assert nothing about that path"
    );
}

#[test]
fn test_delivery_prunes_a_brain_deleted_from_disk_and_still_delivers_the_others() {
    // A Brain removed by a route the daemon did not perform -- a directory
    // deleted by hand -- keeps its index entry, because
    // `forget_schedules_locked` is only reachable from `remove_if_unused`
    // and `archive`. Delivering against that entry writes the Brain back to
    // disk: `append_journal_value` calls `create_dir_all_durable` on the
    // parent before appending, so the directory the operator deleted is
    // recreated by the act of delivering a schedule from it (#383).
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    seed_scheduled_brain(&store, "vanished", 1_000);
    seed_scheduled_brain(&store, "survivor", 1_200);
    let survivor_id = store.snapshot("survivor").unwrap().brain_id;
    let vanished_dir = temp.path().join("vanished");
    assert_eq!(
        store.indexed_schedule_count(),
        2,
        "precondition: both Brains must hold an indexed active schedule, or \
             the pass below observes nothing; index holds {:?}",
        indexed_due_keys(&store)
    );

    std::fs::remove_dir_all(&vanished_dir).unwrap();

    // One delivery pass, in the shape the loop runs it: select from the
    // index, then deliver each selected Brain in turn.
    let selected = store.due_schedule_brains(1_500);
    assert_eq!(
        selected,
        vec!["vanished".to_string(), "survivor".to_string()],
        "precondition: the deleted Brain is due first, so it is the head the \
             pass starts from; index holds {:?}",
        indexed_due_keys(&store)
    );
    let mut delivered = Vec::new();
    for name in &selected {
        let queued = store
            .queue_due_schedules(name, 1_500)
            .unwrap_or_else(|error| {
                panic!(
                    "a delivery pass must survive a Brain that is gone: '{name}' \
                     failed with {error:#}; selection was {selected:?} and the \
                     Brain root holds {}",
                    directory_listing(temp.path())
                )
            });
        delivered.push((name.clone(), queued.len()));
    }

    assert!(
        !vanished_dir.exists(),
        "delivery must not recreate the durable state of a Brain that was \
             deleted: {} was written back holding [{}]. This is the resurrection \
             #383 reports -- the daemon reconstructing a Brain nobody asked for, \
             as a side effect of delivering its stale schedule. Brain root holds \
             [{}]",
        vanished_dir.display(),
        directory_listing(&vanished_dir),
        directory_listing(temp.path())
    );
    assert!(
        !vanished_dir.join("metadata.json").exists(),
        "and specifically no identity may be minted for it: {} exists, which \
             is `load_or_create_metadata` having created a fresh BrainId for a \
             Brain that no longer exists",
        vanished_dir.join("metadata.json").display()
    );
    assert_eq!(
        delivered,
        vec![("vanished".to_string(), 0), ("survivor".to_string(), 1)],
        "the deleted Brain must queue nothing and must not abort the pass; \
             every other Brain's due schedule must still deliver on that same \
             pass. Index now holds {:?}",
        indexed_due_keys(&store)
    );
    assert_eq!(
        store.snapshot("survivor").unwrap().brain_id,
        survivor_id,
        "and the surviving Brain must keep its identity across the pass -- a \
             changed BrainId here would mean delivery had rebuilt it too"
    );
    assert_eq!(
        store.due_schedule_brains(u64::MAX),
        vec!["survivor".to_string()],
        "the deleted Brain's index entries must be gone at every instant, \
             not merely skipped this once; index holds {:?}",
        indexed_due_keys(&store)
    );
    assert_eq!(
        store.indexed_schedule_count(),
        1,
        "exactly the survivor's schedule may remain indexed; index holds {:?}",
        indexed_due_keys(&store)
    );
}

#[test]
fn test_delivery_does_not_rebuild_an_unhydrated_deleted_brain_under_a_new_identity() {
    // The trace in #383, at the point the identity is minted: the index
    // names a Brain the store has not hydrated, delivery calls
    // `ensure_loaded`, `load_or_create_metadata` finds no `metadata.json`
    // and *creates* one, fsyncing a new BrainId into place. The Brain comes
    // back empty, under a different identity, from a directory the operator
    // deleted.
    let temp = tempfile::tempdir().unwrap();
    {
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&store, "ghost", 1_000);
    }
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store.warm_schedule_index();
    let original_id = store.snapshot("ghost").unwrap().brain_id;
    assert_eq!(
        store.due_schedule_brains(1_500),
        vec!["ghost".to_string()],
        "precondition: the warmed index names the Brain; index holds {:?}",
        indexed_due_keys(&store)
    );

    let ghost_dir = temp.path().join("ghost");
    std::fs::remove_dir_all(&ghost_dir).unwrap();
    evict_resident_brain(&store, "ghost");

    let queued = store
        .queue_due_schedules("ghost", 1_500)
        .unwrap_or_else(|error| {
            panic!(
                "delivery against a Brain that is gone must skip it, not \
                     fail: {error:#}; Brain root holds [{}]",
                directory_listing(temp.path())
            )
        });

    assert!(
        queued.is_empty(),
        "a Brain that no longer exists must queue no runs; it queued {} \
             ({:?})",
        queued.len(),
        queued.iter().map(|run| run.run_id).collect::<Vec<_>>()
    );
    assert!(
        !ghost_dir.exists(),
        "delivery must not write the Brain back: {} holds [{}]. Before \
             deletion its identity was {original_id:?}; anything under this path \
             now is a second, empty Brain minted by the delivery pass",
        ghost_dir.display(),
        directory_listing(&ghost_dir)
    );
    assert!(
        !ghost_dir.join("metadata.json").exists(),
        "and no fresh metadata may be fsynced into place for it: {} exists, \
             so the Brain that was {original_id:?} now has a different identity \
             on disk",
        ghost_dir.join("metadata.json").display()
    );
    assert_eq!(
        store.indexed_schedule_count(),
        0,
        "the stale entry must be dropped, or the next pass selects it again \
             and rebuilds the Brain then; index holds {:?}",
        indexed_due_keys(&store)
    );
}

#[test]
fn test_delivery_does_not_prune_a_brain_that_exists_but_cannot_be_replayed() {
    // The distinction the fix turns on. Absent means gone: prune it.
    // Unreadable means a Brain that still exists and cannot be replayed --
    // a corruption to report and repair (#371, #377, #379). Pruning that
    // would silently retire the schedules of a Brain that is still there
    // and hide exactly the failure those issues exist to surface, so the
    // check must be an existence check *before* the load and not "treat any
    // `ensure_loaded` error as prune it".
    let temp = tempfile::tempdir().unwrap();
    {
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&store, "sick", 1_000);
    }
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store.warm_schedule_index();
    assert_eq!(
        store.due_schedule_brains(1_500),
        vec!["sick".to_string()],
        "precondition: the warmed index names the Brain; index holds {:?}",
        indexed_due_keys(&store)
    );

    let sick_dir = temp.path().join("sick");
    std::fs::write(sick_dir.join("metadata.json"), "{not json").unwrap();
    evict_resident_brain(&store, "sick");

    let error = match store.queue_due_schedules("sick", 1_500) {
        Err(error) => error,
        Ok(queued) => panic!(
            "a Brain that exists but cannot be replayed must surface its \
                 fault to the delivery loop, which logs it; delivery instead \
                 returned {} run(s) and the index now holds {:?}",
            queued.len(),
            indexed_due_keys(&store)
        ),
    };

    let reported = format!("{error:#}");
    assert!(
        reported.contains("sick"),
        "the reported fault must name the Brain, or the delivery loop's \
             warning is not actionable by whoever has to repair it; it read: \
             {reported}"
    );
    assert_eq!(
        store.due_schedule_brains(1_500),
        vec!["sick".to_string()],
        "and its schedules must stay indexed: forgetting them would retire \
             a Brain that still exists on disk and would silence the fault on \
             every later pass. Index holds {:?}; the Brain directory holds [{}]",
        indexed_due_keys(&store),
        directory_listing(&sick_dir)
    );
    assert_eq!(
        store.indexed_schedule_count(),
        1,
        "no entry may be dropped for an unreadable Brain; index holds {:?}",
        indexed_due_keys(&store)
    );
    assert!(
        sick_dir.exists(),
        "precondition still holding: the Brain directory is present -- that \
             presence is the whole reason it must not be pruned"
    );
}

#[test]
fn test_pruning_is_idempotent_and_a_restored_brain_is_reindexed_by_the_next_warm() {
    // Pruning must lose nothing permanently. The index is derived from the
    // log, so a directory that comes back -- a volume remounted, a restore
    // completed -- is picked up by the next warm, under its original
    // identity rather than a fresh one.
    let temp = tempfile::tempdir().unwrap();
    {
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&store, "flaky", 1_000);
    }
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store.warm_schedule_index();
    let original_id = store.snapshot("flaky").unwrap().brain_id;
    evict_resident_brain(&store, "flaky");

    // Move the Brain out of the root entirely, the way an unmounted volume
    // takes it away, keeping the bytes so it can come back unchanged.
    let stash = tempfile::tempdir().unwrap();
    let flaky_dir = temp.path().join("flaky");
    let stashed = stash.path().join("flaky");
    std::fs::rename(&flaky_dir, &stashed).unwrap();

    for (pass, now_ms) in [(1u32, 1_500u64), (2, 2_500), (3, 3_500)] {
        let queued = store
            .queue_due_schedules("flaky", now_ms)
            .unwrap_or_else(|error| {
                panic!(
                    "pruning must be idempotent: pass {pass} at {now_ms} ms \
                         failed with {error:#}; Brain root holds [{}]",
                    directory_listing(temp.path())
                )
            });
        assert!(
            queued.is_empty(),
            "pass {pass} queued {} run(s) for a Brain that is not on disk",
            queued.len()
        );
        assert_eq!(
            store.indexed_schedule_count(),
            0,
            "pass {pass} must leave the index empty and must not re-add \
                 anything; index holds {:?}",
            indexed_due_keys(&store)
        );
        assert!(
            !flaky_dir.exists(),
            "pass {pass} must not recreate {}; it holds [{}]",
            flaky_dir.display(),
            directory_listing(&flaky_dir)
        );
    }

    std::fs::rename(&stashed, &flaky_dir).unwrap();
    store.warm_schedule_index();

    assert_eq!(
        store.due_schedule_brains(u64::MAX),
        vec!["flaky".to_string()],
        "a restored Brain must be picked up by the next warm; pruning is a \
             cache eviction, not a deletion. Index holds {:?}; Brain root holds \
             [{}]",
        indexed_due_keys(&store),
        directory_listing(temp.path())
    );
    assert_eq!(
        store.snapshot("flaky").unwrap().brain_id,
        original_id,
        "and it must come back as the same Brain, not as a new one minted \
             during its absence"
    );
}

#[cfg(unix)]
#[test]
fn test_delivery_does_not_prune_a_brain_whose_existence_cannot_be_determined() {
    // `Path::exists` is `fs::metadata(..).is_ok()`, so it answers `false`
    // for *every* error, not only for ENOENT. An EACCES on the Brain root,
    // an ESTALE from a stale NFS or SMB handle, an EIO from a failing disk
    // -- each of those reads as "the Brain is gone" and would prune the
    // schedules of a Brain that is sitting right there, silently, with the
    // delivery returning `Ok`. That is the exact silencing this fix exists
    // to prevent, produced by the branch written to prevent it. `absent`
    // and `unreadable` is the distinction the whole design rests on, and an
    // errored existence check belongs on the `unreadable` side: it is not
    // evidence of absence.
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    seed_scheduled_brain(&store, "alpha", 1_000);
    let original_id = store.snapshot("alpha").unwrap().brain_id;
    let alpha_dir = temp.path().join("alpha");
    // Unhydrated, so the load actually reaches the filesystem rather than
    // answering from the resident copy.
    evict_resident_brain(&store, "alpha");
    assert_eq!(
        store.due_schedule_brains(1_500),
        vec!["alpha".to_string()],
        "precondition: the Brain's schedule is indexed and due; index holds \
             {:?}",
        indexed_due_keys(&store)
    );

    // Restores the mode however this scope is left. Three statements run
    // with the root at 0o000, one of them a hard assertion; a panic between
    // them must not leave an unreadable directory behind for `tempfile` to
    // fail to clean up.
    struct RestoreMode<'a>(&'a std::path::Path, u32);
    impl Drop for RestoreMode<'_> {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(self.1));
        }
    }

    let original_mode = std::fs::metadata(temp.path()).unwrap().permissions().mode();
    let restore = RestoreMode(temp.path(), original_mode);
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
    // A process not subject to the mode -- root, or a filesystem that does
    // not enforce permission bits -- cannot reach the state under test at
    // all, and a green result here would then claim coverage that does not
    // exist. Fail rather than return: a silent pass is the failure mode
    // this whole fix is about.
    assert!(
        std::fs::read_dir(temp.path()).is_err(),
        "this regression cannot exercise its subject here: {} is still \
             readable at mode 0o000, so an unreadable Brain root -- the entire \
             state under test -- is unreachable and passing would assert \
             nothing. Run the suite as an unprivileged user on a filesystem \
             that enforces permission bits; running as root, or on a mount \
             without permission enforcement, makes this test vacuous",
        temp.path().display()
    );
    let outcome = store.queue_due_schedules("alpha", 1_500);
    let indexed_while_unreadable = indexed_due_keys(&store);
    let selected_while_unreadable = store.due_schedule_brains(1_500);
    // Restore before asserting: the assertions below have to read the root.
    drop(restore);

    let error = match outcome {
        Err(error) => error,
        Ok(queued) => panic!(
            "an existence check that could not answer must not be read as \
                 'the Brain is gone': the root was unreadable, not empty, and \
                 {} was on disk the whole time. Delivery instead returned {} \
                 run(s) and the index now holds {indexed_while_unreadable:?} -- \
                 a Brain that still exists had its schedules retired with no \
                 fault reported anywhere",
            alpha_dir.display(),
            queued.len()
        ),
    };

    let reported = format!("{error:#}");
    assert!(
        reported.contains("alpha"),
        "the reported fault must name the Brain, or the delivery loop's \
             warning is not actionable by whoever has to repair the permissions \
             or remount the volume; it read: {reported}"
    );
    assert!(
        alpha_dir.exists(),
        "precondition still holding: nothing was deleted -- {} holds [{}]. \
             The directory's presence throughout is the whole reason pruning it \
             would have been wrong",
        alpha_dir.display(),
        directory_listing(&alpha_dir)
    );
    assert_eq!(
        selected_while_unreadable,
        vec!["alpha".to_string()],
        "and its schedules must stay indexed and selectable while the root \
             is unreadable, so the fault is reported again on the next pass \
             instead of being forgotten. Index held {indexed_while_unreadable:?}"
    );
    assert_eq!(
        indexed_while_unreadable.len(),
        1,
        "no entry may be dropped when the existence check itself failed; \
             index held {indexed_while_unreadable:?}"
    );
    assert_eq!(
        store.snapshot("alpha").unwrap().brain_id,
        original_id,
        "and once the root is readable again the Brain must still be the \
             same one, not a fresh identity minted while it could not be stat'd"
    );
}

#[test]
fn test_a_resident_brain_is_reindexed_by_the_next_warm_after_its_root_blinked_out() {
    // Pruning removes index entries; nothing re-adds them for a Brain that
    // is still *resident*. `ensure_loaded` returns `Ok(())` at its first
    // line when `brains` already holds the key, before the
    // `reindex_schedules_locked` on its load path, and `warm_schedule_index`
    // did nothing but call `ensure_loaded` per directory. Entries entered
    // the index only when a Brain became resident or when a schedule event
    // fired, so a resident-but-unindexed Brain had no route back short of a
    // process restart.
    //
    // That is not a corner: the warm hydrates every Brain at startup, so
    // every Brain is resident. A Brain root on a removable or network
    // volume that drops for two seconds would have every due Brain pruned,
    // and when the volume returned the next warm would hydrate nothing new
    // -- leaving the daemon delivering no scheduled work again, ever,
    // silently, until it was restarted. On base a root blip costs nothing.
    //
    // The sibling test evicts before pruning; this one deliberately does
    // not, because "still resident" is the case with no repair.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brains");
    let store = BrainStore::with_root("box.local", Some(root.clone()));
    seed_scheduled_brain(&store, "alpha", 1_000);
    seed_scheduled_brain(&store, "beta", 1_200);
    let alpha_id = store.snapshot("alpha").unwrap().brain_id;
    let beta_id = store.snapshot("beta").unwrap().brain_id;
    assert!(
        store.is_resident_for_tests("alpha") && store.is_resident_for_tests("beta"),
        "precondition: both Brains are resident, which is what the startup \
             warm leaves behind and is exactly the case with no repair path"
    );
    assert_eq!(
        store.indexed_schedule_count(),
        2,
        "precondition: both schedules are indexed; index holds {:?}",
        indexed_due_keys(&store)
    );

    // The whole root goes away and comes back: a volume unmounted for a
    // couple of seconds, which is the reported shape of this failure.
    let stashed = temp.path().join("brains-unmounted");
    std::fs::rename(&root, &stashed).unwrap();

    for name in store.due_schedule_brains(1_500) {
        let queued = store
            .queue_due_schedules(&name, 1_500)
            .unwrap_or_else(|error| {
                panic!(
                    "a pass over a vanished root must not fail: '{name}' failed \
                     with {error:#}"
                )
            });
        assert!(
            queued.is_empty(),
            "'{name}' queued {} run(s) while the Brain root was gone",
            queued.len()
        );
    }
    assert_eq!(
        store.indexed_schedule_count(),
        0,
        "precondition for what follows: the blip pruned every entry; index \
             holds {:?}",
        indexed_due_keys(&store)
    );
    assert!(
        store.is_resident_for_tests("alpha") && store.is_resident_for_tests("beta"),
        "and both Brains are still resident -- pruning drops index entries, \
             not the in-memory state. This is precisely the state with no route \
             back: `ensure_loaded` short-circuits on residency before it would \
             reindex"
    );

    std::fs::rename(&stashed, &root).unwrap();
    store.warm_schedule_index();

    assert_eq!(
        store.due_schedule_brains(u64::MAX),
        vec!["alpha".to_string(), "beta".to_string()],
        "the warm after the root returned must restore every pruned entry, \
             resident or not. It holds {:?}; the Brain root holds [{}]. An empty \
             index here is a daemon that has silently stopped delivering all \
             scheduled work until it is restarted",
        indexed_due_keys(&store),
        directory_listing(&root)
    );
    assert_eq!(
        store.indexed_schedule_count(),
        2,
        "both schedules, not merely one; index holds {:?}",
        indexed_due_keys(&store)
    );
    assert_eq!(
        (
            store.snapshot("alpha").unwrap().brain_id,
            store.snapshot("beta").unwrap().brain_id
        ),
        (alpha_id, beta_id),
        "and both must come back as the same Brains, not as fresh \
             identities minted while the root was away"
    );
}

#[test]
fn test_a_schedule_created_during_the_prune_gap_is_restored_by_the_next_warm() {
    // `prune_schedules_if_brain_is_absent` stats the Brain root *outside*
    // the index write guard, so a `create_schedule` can land between the
    // check and the forget and have its brand-new index entry dropped. That
    // shape is deliberate -- holding the guard across an unbounded `stat`
    // lets one wedged NFS mount freeze every `brains.write()` caller in the
    // process -- and this test is the price: the loss must be **bounded**,
    // repaired by the very next warm without operator action, or the trade
    // is not the one the comment claims.
    //
    // The gap has no lock and no other observable boundary, so a `Barrier`
    // cannot be placed inside it from outside; `run_with_prune_gap_hook` is
    // a `#[cfg(test)]` hook in exactly that window, and the racing creation
    // runs on a second thread while the pruning thread is parked in it.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brains");
    let store = BrainStore::with_root("box.local", Some(root.clone()));
    let (attachment_id, seeded_schedule) = seed_scheduled_brain(&store, "alpha", 1_000);
    let alpha_id = store.snapshot("alpha").unwrap().brain_id;
    let alpha_dir = root.join("alpha");

    // The Brain's directory goes away underneath the daemon: the #383
    // premise (schedules for a deleted Brain are delivered, and delivery
    // recreates the Brain).
    std::fs::remove_dir_all(&alpha_dir).unwrap();

    // Channels rather than a `Barrier`, because every rendezvous needs a
    // watchdog: a two-thread test whose interleaving stops happening must
    // *fail*, not hang. If the existence check ever moves back under the
    // index guard, the creation blocks on a lock the parked prune is
    // holding and a `Barrier` would deadlock the whole suite -- the very
    // stall this shape exists to avoid, reported as a timeout instead of a
    // diagnosis. These durations are liveness watchdogs, never behavioural
    // thresholds: nothing is asserted about how long anything took, and
    // every instant fed to the store is a literal (#242, assert hydration
    // state rather than a wall-clock ratio).
    const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(30);
    let (gap_tx, gap_rx) = std::sync::mpsc::channel::<()>();
    let (created_tx, created_rx) = std::sync::mpsc::channel::<()>();

    let creator = {
        let store = store.clone();
        std::thread::spawn(move || {
            // The prune has stat'd, seen nothing, and parked before it takes
            // the index guard. The gap is open.
            let gap_observed = gap_rx.recv_timeout(WATCHDOG).is_ok();
            let created = store
                .create_schedule(
                    "alpha",
                    "alice",
                    attachment_id,
                    ProgramLanguage::Lisp,
                    "(say \"raced\")",
                    crate::vm::EffectSet::pure(),
                    2_000,
                    Some(1_000),
                    BrainScheduleDeliveryPolicy::Coalesce,
                )
                .expect(
                    "the racing creation must succeed on its own terms: it \
                         recreates the directory through `append_journal_value` \
                         -> `create_dir_all_durable` before it indexes anything",
                );
            // Durable and indexed. Only now does the prune take the guard,
            // so it forgets an entry that provably existed.
            let _ = created_tx.send(());
            (gap_observed, created.schedule_id)
        })
    };

    let queued = run_with_prune_gap_hook(
        Box::new(move || {
            let _ = gap_tx.send(());
            created_rx.recv_timeout(WATCHDOG).expect(
                "the racing creation did not finish inside the prune gap. If \
                     it is blocked rather than slow, the existence check is \
                     holding a lock the creation needs -- which is exactly the \
                     process-wide stall this shape was chosen to avoid",
            );
        }),
        || store.queue_due_schedules("alpha", 1_500),
    )
    .expect("a pass over a Brain whose directory is gone must not fail");
    let (gap_observed, raced_schedule) = creator.join().expect("the racing creation panicked");
    assert!(
        gap_observed,
        "the prune gap never opened: `queue_due_schedules` did not reach the \
             existence check in `prune_schedules_if_brain_is_absent`, so nothing \
             below is testing the race it claims to test"
    );

    assert!(
        queued.is_empty(),
        "the pass must still prune rather than deliver: it queued {} run(s)",
        queued.len()
    );
    assert!(
        alpha_dir.exists(),
        "precondition for the repair: the racing creation put the directory \
             back, so the next warm can find it. {} holds [{}]",
        alpha_dir.display(),
        directory_listing(&alpha_dir)
    );
    let durable_schedules = store
        .snapshot("alpha")
        .unwrap()
        .schedules
        .iter()
        .map(|schedule| schedule.schedule_id)
        .collect::<Vec<_>>();
    assert!(
        durable_schedules.contains(&raced_schedule),
        "and the raced schedule is durable regardless -- only the *index* \
             entry is at stake here. The Brain holds {durable_schedules:?}"
    );

    // The cost of statting outside the guard, stated rather than hidden:
    // the entry the creation had just indexed is gone.
    assert_eq!(
        indexed_due_keys(&store),
        Vec::new(),
        "the documented cost of the outside-the-guard check is exactly this \
             and no more: the prune forgets the Brain wholesale, including the \
             entry `create_schedule` had already inserted"
    );

    // The bound. One warm, no operator action, no restart.
    store.warm_schedule_index();

    let restored = indexed_due_keys(&store)
        .into_iter()
        .map(|(_, _, schedule_id)| schedule_id)
        .collect::<std::collections::HashSet<_>>();
    assert!(
        restored.contains(&raced_schedule),
        "the schedule created during the prune gap must be selectable again \
             after a single warm -- otherwise the loss is unbounded and the \
             trade the check's comment makes is not the one it describes. The \
             index holds {:?}; the Brain root holds [{}]",
        indexed_due_keys(&store),
        directory_listing(&root)
    );
    assert!(
        restored.contains(&seeded_schedule),
        "and so must the schedule that was already there before the race; \
             the index holds {:?}",
        indexed_due_keys(&store)
    );
    assert_eq!(
        store.due_schedule_brains(u64::MAX),
        vec!["alpha".to_string()],
        "and the Brain must be selectable by the delivery loop again, not \
             merely present in the index; it holds {:?}",
        indexed_due_keys(&store)
    );

    let delivered = store
        .queue_due_schedules("alpha", 9_000)
        .expect("delivery after the repair must succeed");
    assert_eq!(
        delivered.len(),
        2,
        "and delivery must actually run both schedules on the next due pass \
             -- an index entry that never reaches `queue_due_schedules` is not a \
             repair. Index holds {:?}",
        indexed_due_keys(&store)
    );
    assert_eq!(
        store.snapshot("alpha").unwrap().brain_id,
        alpha_id,
        "and none of this may mint a new identity: the Brain that was \
             pruned and the Brain that was repaired must be the same one"
    );
}

#[test]
fn test_a_vanished_brain_is_selected_once_rather_than_on_every_delivery_pass() {
    // The head of the index naming a Brain that no longer exists must not
    // keep the loop working. Skipping without pruning would leave the entry
    // due forever: the head never advances, every pass selects it again,
    // and the loop settles into `should_back_off`'s one-second retry on a
    // Brain that can never be delivered. Asserted structurally, by counting
    // selections over a bounded number of passes -- never by timing (#242).
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    seed_scheduled_brain(&store, "vanished", 1_000);
    seed_scheduled_brain(&store, "survivor", 1_200);
    let vanished_dir = temp.path().join("vanished");
    std::fs::remove_dir_all(&vanished_dir).unwrap();

    const PASSES: u32 = 5;
    let mut vanished_selections = 0u32;
    let mut survivor_selections = 0u32;
    let mut selections_by_pass = Vec::new();
    for pass in 0..PASSES {
        let now_ms = 1_500 + u64::from(pass) * 1_000;
        let selected = store.due_schedule_brains(now_ms);
        for name in &selected {
            if name == "vanished" {
                vanished_selections += 1;
            } else {
                survivor_selections += 1;
            }
            let _ = store.queue_due_schedules(name, now_ms);
        }
        selections_by_pass.push((now_ms, selected));
        assert!(
            !vanished_dir.exists(),
            "no pass may write the deleted Brain back to disk; after the \
                 pass at {now_ms} ms {} holds [{}]",
            vanished_dir.display(),
            directory_listing(&vanished_dir)
        );
    }

    assert_eq!(
        vanished_selections, 1,
        "a Brain that is gone must be selected once -- the pass that prunes \
             it -- and never again. It was selected {vanished_selections} times \
             across {PASSES} passes: {selections_by_pass:?}. More than one means \
             the stale entry survived delivery, so the loop keeps doing work for \
             a Brain that cannot be delivered"
    );
    assert_eq!(
        survivor_selections, PASSES,
        "and the pruning must not cost the healthy Brain any of its \
             occurrences: it was selected {survivor_selections} times across \
             {PASSES} passes: {selections_by_pass:?}"
    );
    assert_eq!(
        store.indexed_schedule_count(),
        1,
        "only the survivor's schedule may remain indexed after the passes; \
             index holds {:?}",
        indexed_due_keys(&store)
    );
}

/// Creating a schedule that becomes the new head must wake the delivery
/// loop, and creating one that does not must leave it asleep.
///
/// The previous version of this test called `notify_one` itself on a handle
/// it already held, so both sides of the assertion came from the test. It
/// asserted `tokio::sync::Notify`'s contract and left the production call
/// site untouched: reverting it to `notify_waiters` -- the exact defect it
/// was written to prevent -- kept it green. This one never notifies; only
/// `reindex_schedules_locked` can.
#[test]
fn test_creating_an_earlier_schedule_wakes_the_delivery_loop() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    // Take the handle the delivery loop would take, before anything is
    // indexed, so a mutation that hands out a fresh `Notify` per call is
    // also caught.
    let wakeup = store.schedule_wakeup();
    seed_scheduled_brain(&store, "later", 900_000);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();

    // The first schedule into an empty index is itself a head move, so it
    // wakes -- and `notify_one` stores that permit. Assert it and consume
    // it, or the negative case below reads this permit and fails. (It did,
    // the first time this test ran, which is the coupling to production the
    // previous version of this test lacked entirely.)
    let woke_for_first = runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), wakeup.notified())
            .await
            .is_ok()
    });
    assert!(
        woke_for_first,
        "the first schedule into an empty index moves the head from None \
             and must wake the loop, which would otherwise be sleeping the full \
             60 s ceiling with nothing indexed"
    );

    // A schedule further out than the head must not wake anything: an
    // unnecessary wake is a spin, and `moved_earlier` returning true
    // unconditionally would show up here.
    seed_scheduled_brain(&store, "further", 950_000);
    let woke_for_later = runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_millis(200), wakeup.notified())
            .await
            .is_ok()
    });
    assert!(
        !woke_for_later,
        "a schedule due after the current head must not interrupt a sleep \
             it does not shorten; head was 900000 and the new schedule was \
             950000"
    );

    // A schedule earlier than the head must wake, and the notification has
    // to survive being sent while nothing is waiting -- which is the
    // window the delivery loop sits in between sampling the head and
    // registering. `notify_waiters` drops it; `notify_one` stores a permit.
    seed_scheduled_brain(&store, "sooner", 1_000);
    let woke_for_sooner = runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), wakeup.notified())
            .await
            .is_ok()
    });
    assert!(
        woke_for_sooner,
        "a schedule that becomes the new head must wake the delivery loop, \
             and must do so even though the wake was sent while no waiter was \
             registered -- otherwise it waits out the existing sleep, up to the \
             60 s ceiling, silently. Head is {:?}",
        store.next_schedule_due_ms()
    );
}

/// Selecting due work must not hydrate Brains that have none.
///
/// Asserted with `resident_brain_count`, as #374 required -- never a
/// duration. The previous version asserted only that selection *named* one
/// Brain, which `due_schedule_brains` cannot get wrong: it is a pure read
/// of the index and cannot hydrate anything. That left the property
/// tautological, and a `due_schedule_brains` that called `list()` first --
/// hydrating all 65 Brains on every tick, the exact defect #374 removes --
/// would have passed it.
#[test]
fn test_a_repaired_brain_is_picked_up_by_a_later_warm() {
    // The regression for round 2's worst finding. Warming once meant a
    // Brain that happened to be unloadable at daemon start -- a volume not
    // yet mounted, a journal mid-repair -- was never scheduled again for
    // the life of the daemon, silently, because a scheduled Brain is
    // precisely the one nothing else touches by name to hydrate it. The
    // fixed one-second tick this replaced recovered within a second.
    //
    // Asserted at the store, with no clock and no loop: the property the
    // periodic re-warm depends on is that warming again picks up a Brain
    // that has since become loadable.
    let temp = tempfile::tempdir().unwrap();
    let good_metadata = {
        let seeding = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&seeding, "healthy", 1_000);
        seed_scheduled_brain(&seeding, "repairable", 2_000);
        std::fs::read(temp.path().join("repairable").join("metadata.json")).unwrap()
    };

    // Break it the way the reference host's Brain was broken: unreadable
    // identity, so `ensure_loaded` refuses it.
    std::fs::write(
        temp.path().join("repairable").join("metadata.json"),
        "{not json",
    )
    .unwrap();

    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store.warm_schedule_index();
    assert_eq!(
        store.due_schedule_brains(5_000),
        vec!["healthy".to_string()],
        "precondition: the broken Brain is skipped and the healthy one is \
             still scheduled, which is what keeps one bad Brain from stopping \
             delivery for every Brain"
    );

    std::fs::write(
        temp.path().join("repairable").join("metadata.json"),
        good_metadata,
    )
    .unwrap();

    store.warm_schedule_index();

    assert_eq!(
        store.due_schedule_brains(5_000),
        vec!["healthy".to_string(), "repairable".to_string()],
        "a Brain that has become loadable must be picked up by a later \
             warm. Warming only once leaves it unscheduled until the daemon \
             restarts, with a single startup warning as the only symptom -- \
             and nothing else will index it, because a scheduled Brain is the \
             one nothing touches by name"
    );
}

#[test]
fn test_selection_does_not_hydrate_brains_without_due_work() {
    const IDLE: usize = 64;
    let temp = tempfile::tempdir().unwrap();
    {
        let seeding = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&seeding, "has-work", 1_000);
        for index in 0..IDLE {
            seeding.snapshot(&format!("idle-{index:04}")).unwrap();
        }
    }

    // Deliberately not warmed: warming hydrates everything it can, which is
    // the one enumeration #374 keeps. What is under test is the steady
    // state, where the index already knows the due Brain and selection must
    // touch nothing else.
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store.snapshot("has-work").unwrap();
    assert_eq!(
        store.resident_brain_count(),
        1,
        "precondition: exactly the Brain with work is resident, out of \
             {} on disk",
        IDLE + 1
    );

    let due = store.due_schedule_brains(2_000);

    assert_eq!(
        due,
        vec!["has-work".to_string()],
        "only the Brain with due work may be selected"
    );
    assert_eq!(
        store.resident_brain_count(),
        1,
        "selecting due work must hydrate nothing. {} Brains are on disk and \
             exactly one has a due schedule; a selection path that enumerated \
             or replayed the root to answer would leave them resident here, \
             which is the per-second cost #374 exists to remove",
        IDLE + 1
    );
}

#[test]
fn test_two_schedules_in_one_brain_name_it_once() {
    // `due_brains` dedups, because the delivery loop hydrates and locks per
    // name. Without the dedup a Brain with two due schedules is processed
    // twice per tick, taking its execution lock twice for one pass.
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let (attachment, _) = seed_scheduled_brain(&store, "busy", 1_000);
    store
        .create_schedule(
            "busy",
            "alice",
            attachment,
            ProgramLanguage::Lisp,
            "(say \"second\")",
            crate::vm::EffectSet::pure(),
            1_500,
            Some(1_000),
            BrainScheduleDeliveryPolicy::Coalesce,
        )
        .unwrap();
    assert_eq!(store.indexed_schedule_count(), 2, "two schedules indexed");

    assert_eq!(
        store.due_schedule_brains(2_000),
        vec!["busy".to_string()],
        "a Brain with two due schedules must be named once; naming it twice \
             makes the delivery loop take its execution lock twice for one pass"
    );
}

#[test]
fn test_due_order_holds_across_more_than_two_brains() {
    // Two-element orderings pass by chance about half the time against a
    // hash-ordered mutation. Four, in an order that matches neither name
    // order nor insertion order, does not.
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    seed_scheduled_brain(&store, "delta", 4_000);
    seed_scheduled_brain(&store, "alpha", 2_000);
    seed_scheduled_brain(&store, "charlie", 1_000);
    seed_scheduled_brain(&store, "bravo", 3_000);

    assert_eq!(
        store.due_schedule_brains(10_000),
        vec![
            "charlie".to_string(),
            "alpha".to_string(),
            "bravo".to_string(),
            "delta".to_string(),
        ],
        "due order must follow next_due_ms across every Brain. This order \
             matches neither name order (alpha, bravo, charlie, delta) nor \
             insertion order (delta, alpha, charlie, bravo), so a hash-ordered \
             or enumeration-ordered result cannot produce it by chance"
    );
}

#[test]
fn test_the_warmed_index_agrees_with_the_log_after_a_restart_mid_flight() {
    // The "one authoritative store" claim rests on this: the index is
    // derived, so after a restart with a fired-but-outstanding occurrence
    // the warmed head must equal what the event log replays to, not what it
    // was before the fire.
    let temp = tempfile::tempdir().unwrap();
    let advanced = {
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&store, "midflight", 1_000);
        let queued = store.queue_due_schedules("midflight", 1_500).unwrap();
        assert_eq!(
            queued.len(),
            1,
            "the schedule fired, leaving a due outstanding"
        );
        let head = store.next_schedule_due_ms();
        assert_eq!(head, Some(2_000), "and advanced by its interval");
        head
    };

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    restarted.warm_schedule_index();

    assert_eq!(
        restarted.next_schedule_due_ms(),
        advanced,
        "after a restart with an occurrence outstanding, the warmed index \
             must agree with the replayed log. Disagreeing means the daemon \
             would either re-fire an occurrence it already queued or skip the \
             next one"
    );
    let snapshot = restarted.snapshot("midflight").unwrap();
    assert_eq!(
        snapshot.schedules[0].next_due_ms,
        advanced.unwrap(),
        "and the log itself must carry that same next_due_ms, so the index \
             is derived from it rather than merely consistent with itself"
    );
}

#[test]
fn schedule_due_is_one_durable_coalesced_run_until_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .create_schedule(
            "shared",
            "alice",
            attachment.attachment_id,
            ProgramLanguage::Lisp,
            "(say \"tick\")",
            crate::vm::EffectSet::pure(),
            1_000,
            Some(1_000),
            BrainScheduleDeliveryPolicy::Coalesce,
        )
        .unwrap();
    let queued = store.queue_due_schedules("shared", 3_500).unwrap();
    assert_eq!(queued.len(), 1);
    let run = queued[0].clone();
    let first = store.snapshot("shared").unwrap();
    let first_due = first.pending_schedule_dues[0].clone();
    assert_eq!(first_due.missed_count, 3);
    assert_eq!(first_due.due_at_ms, 3_000);
    assert_eq!(first_due.next_due_ms, Some(4_000));
    assert_eq!(run.request_seq, first.revision);
    assert!(store
        .queue_due_schedules("shared", 3_500)
        .unwrap()
        .is_empty());

    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.snapshot("shared").unwrap();
    assert_eq!(restored.schedules[0].next_due_ms, 4_000);
    assert_eq!(restored.pending_schedule_dues, vec![first_due]);
    assert_eq!(restored.runs, vec![run.clone()]);

    let requeued = restarted.queue_due_schedules("shared", 5_500).unwrap();
    assert_eq!(requeued.len(), 1);
    assert_eq!(requeued[0].run_id, run.run_id);
    let snapshot = restarted.snapshot("shared").unwrap();
    assert_eq!(snapshot.pending_schedule_dues[0].missed_count, 5);
    assert_eq!(snapshot.pending_schedule_dues[0].next_due_ms, Some(6_000));
    assert_eq!(
        snapshot.runs.len(),
        1,
        "coalescing must reuse the queued run"
    );

    restarted
        .transition_run(
            "shared",
            "runner",
            run.run_id,
            BrainRunStatus::Running,
            None,
        )
        .unwrap();
    restarted
        .transition_run(
            "shared",
            "runner",
            run.run_id,
            BrainRunStatus::Completed,
            None,
        )
        .unwrap();
    assert!(restarted
        .snapshot("shared")
        .unwrap()
        .pending_schedule_dues
        .is_empty());
}

#[test]
fn bounded_catch_up_limits_pending_runs_and_skips_expired_ticks() {
    let store = BrainStore::with_root("box.local", None);
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .create_schedule(
            "shared",
            "alice",
            attachment.attachment_id,
            ProgramLanguage::Forth,
            "\"tick\" say",
            crate::vm::EffectSet::pure(),
            1_000,
            Some(1_000),
            BrainScheduleDeliveryPolicy::BoundedCatchUp {
                max_catch_up: 2,
                expires_after_ms: 2_500,
            },
        )
        .unwrap();

    let queued = store.queue_due_schedules("shared", 5_000).unwrap();
    assert_eq!(queued.len(), 2);
    let snapshot = store.snapshot("shared").unwrap();
    assert_eq!(
        snapshot
            .pending_schedule_dues
            .iter()
            .map(|due| due.due_at_ms)
            .collect::<Vec<_>>(),
        vec![3_000, 4_000]
    );
    assert_eq!(snapshot.schedules[0].next_due_ms, 5_000);
    assert!(store
        .queue_due_schedules("shared", 5_000)
        .unwrap()
        .is_empty());

    store
        .transition_run(
            "shared",
            "runner",
            queued[0].run_id,
            BrainRunStatus::Running,
            None,
        )
        .unwrap();
    store
        .transition_run(
            "shared",
            "runner",
            queued[0].run_id,
            BrainRunStatus::Completed,
            None,
        )
        .unwrap();
    let next = store.queue_due_schedules("shared", 5_000).unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(
        store
            .snapshot("shared")
            .unwrap()
            .pending_schedule_dues
            .len(),
        2
    );
}

#[test]
fn schedule_inspection_and_cancellation_are_durable_and_creator_bound() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let sibling = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let schedule = store
        .create_schedule(
            "shared",
            "alice",
            attachment.attachment_id,
            ProgramLanguage::Lisp,
            "(say \"later\")",
            crate::vm::EffectSet::pure(),
            10_000,
            None,
            BrainScheduleDeliveryPolicy::Coalesce,
        )
        .unwrap();
    assert_eq!(
        store
            .inspect_schedule("shared", schedule.schedule_id)
            .unwrap(),
        Some(schedule.clone())
    );
    assert!(store
        .cancel_schedule(
            "shared",
            "bob",
            attachment.attachment_id,
            schedule.schedule_id,
        )
        .unwrap_err()
        .to_string()
        .contains("only the schedule creator attachment"));
    assert!(store
        .cancel_schedule(
            "shared",
            "alice",
            sibling.attachment_id,
            schedule.schedule_id,
        )
        .unwrap_err()
        .to_string()
        .contains("only the schedule creator attachment"));
    assert!(store
        .cancel_schedule(
            "shared",
            "alice",
            attachment.attachment_id,
            schedule.schedule_id,
        )
        .unwrap());
    assert!(!store
        .cancel_schedule(
            "shared",
            "alice",
            attachment.attachment_id,
            schedule.schedule_id,
        )
        .unwrap());
    assert!(store
        .queue_due_schedules("shared", 20_000)
        .unwrap()
        .is_empty());

    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert!(
        !restarted
            .inspect_schedule("shared", schedule.schedule_id)
            .unwrap()
            .unwrap()
            .active
    );
}

#[test]
fn run_lifecycle_is_event_sourced_and_terminal_state_is_final() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let prompt = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "inspect".into(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            prompt.seq,
            attachment.attachment_id,
            BrainRunStatus::QueuedForEnvironment,
        )
        .unwrap();
    store
        .transition_run(
            "shared",
            "runner",
            run.run_id,
            BrainRunStatus::Running,
            None,
        )
        .unwrap();
    store
        .transition_run(
            "shared",
            "runner",
            run.run_id,
            BrainRunStatus::Completed,
            None,
        )
        .unwrap();
    assert!(store
        .transition_run(
            "shared",
            "runner",
            run.run_id,
            BrainRunStatus::Running,
            None,
        )
        .is_err());

    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = &restarted.snapshot("shared").unwrap().runs[0];
    assert_eq!(restored.run_id, run.run_id);
    assert_eq!(restored.request_seq, prompt.seq);
    assert_eq!(restored.status, BrainRunStatus::Completed);
    assert!(restored.updated_ms >= restored.started_ms);
}

#[test]
fn explicit_cancel_racing_disconnect_never_publishes_result_after_terminal() {
    let store = BrainStore::with_root("box.local", None);
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let prompt = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "race".into(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            prompt.seq,
            attachment.attachment_id,
            BrainRunStatus::Running,
        )
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let cancel_store = store.clone();
    let cancel_barrier = barrier.clone();
    let run_id = run.run_id;
    let cancel = std::thread::spawn(move || {
        cancel_barrier.wait();
        cancel_store.transition_run(
            "shared",
            "alice",
            run_id,
            BrainRunStatus::Cancelled,
            Some("cancelled by initiating driver".into()),
        )
    });
    let disconnect_store = store.clone();
    let disconnect_barrier = barrier.clone();
    let disconnect = std::thread::spawn(move || {
        disconnect_barrier.wait();
        disconnect_store.terminalize_run_with_result_if_active(
            "shared",
            "daemon",
            run_id,
            prompt.seq,
            BrainRunStatus::Failed,
            "initiating Brain connection disconnected".into(),
        )
    });
    barrier.wait();
    let _ = cancel.join().unwrap();
    disconnect.join().unwrap().unwrap();

    let snapshot = store.snapshot("shared").unwrap();
    let terminal = snapshot
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                BrainEventKind::RunStatusChanged { run_id: event_run_id, status, .. }
                    if event_run_id == run_id && status.is_terminal()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 1);
    assert!(!snapshot.events.iter().any(|event| {
        event.seq > terminal[0].seq
            && event.run_id == Some(run_id)
            && matches!(event.kind, BrainEventKind::Result { .. })
    }));
    assert!(
        snapshot
            .events
            .iter()
            .filter(|event| {
                event.run_id == Some(run_id) && matches!(event.kind, BrainEventKind::Result { .. })
            })
            .count()
            <= 1
    );
}

#[test]
fn disconnect_terminal_batch_is_all_or_nothing_across_failure_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let prompt = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "crash".into(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            prompt.seq,
            attachment.attachment_id,
            BrainRunStatus::Running,
        )
        .unwrap();
    let before = store.snapshot("shared").unwrap().revision;
    store.fail_next_event_batch_for_test();
    assert!(store
        .terminalize_run_with_result_if_active(
            "shared",
            "daemon",
            run.run_id,
            prompt.seq,
            BrainRunStatus::Failed,
            "initiating Brain connection disconnected".into(),
        )
        .is_err());
    let failed_append = store.snapshot("shared").unwrap();
    assert_eq!(failed_append.revision, before);
    assert_eq!(
        failed_append
            .runs
            .iter()
            .find(|candidate| { candidate.run_id == run.run_id })
            .unwrap()
            .status,
        BrainRunStatus::Running
    );
    assert!(!failed_append.events.iter().any(|event| {
        event.run_id == Some(run.run_id) && matches!(event.kind, BrainEventKind::Result { .. })
    }));

    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let recovered = restarted.snapshot("shared").unwrap();
    assert_eq!(
        recovered
            .runs
            .iter()
            .find(|candidate| { candidate.run_id == run.run_id })
            .unwrap()
            .status,
        BrainRunStatus::Failed
    );
    assert_eq!(
        recovered
            .events
            .iter()
            .filter(|event| {
                event.run_id == Some(run.run_id)
                    && matches!(event.kind, BrainEventKind::Result { .. })
            })
            .count(),
        1
    );
    assert_eq!(
        recovered
            .events
            .iter()
            .filter(|event| matches!(
                event.kind,
                BrainEventKind::RunStatusChanged { run_id: event_run_id, status, .. }
                    if event_run_id == run.run_id && status.is_terminal()
            ))
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn disconnect_terminal_retry_owner_survives_extended_transient_failure() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let prompt = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "retry".into(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            prompt.seq,
            attachment.attachment_id,
            BrainRunStatus::Running,
        )
        .unwrap();
    store.fail_event_batches_for_test(12);
    let detail = "initiating Brain connection disconnected".to_string();
    assert!(store
        .terminalize_run_with_result_if_active(
            "shared",
            "daemon",
            run.run_id,
            prompt.seq,
            BrainRunStatus::Failed,
            detail.clone(),
        )
        .is_err());
    store.schedule_disconnect_terminalization_retry(
        "shared".into(),
        "daemon".into(),
        run.run_id,
        prompt.seq,
        BrainRunStatus::Failed,
        detail.clone(),
    );
    store.schedule_disconnect_terminalization_retry(
        "shared".into(),
        "daemon".into(),
        run.run_id,
        prompt.seq,
        BrainRunStatus::Failed,
        detail,
    );
    assert_eq!(store.pending_disconnect_terminalization_retries(), 1);
    let publication = store
        .acquire_run_publication("shared", run.run_id)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(store.pending_disconnect_terminalization_retries(), 1);
    assert_eq!(
        store.inspect_run("shared", run.run_id).unwrap().status,
        BrainRunStatus::Running
    );
    drop(publication);
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            if store.inspect_run("shared", run.run_id).unwrap().status == BrainRunStatus::Failed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(store.pending_disconnect_terminalization_retries(), 0);
    let snapshot = store.snapshot("shared").unwrap();
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|event| {
                event.run_id == Some(run.run_id)
                    && matches!(event.kind, BrainEventKind::Result { .. })
            })
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|event| matches!(
                event.kind,
                BrainEventKind::RunStatusChanged { run_id: event_run_id, status, .. }
                    if event_run_id == run.run_id && status.is_terminal()
            ))
            .count(),
        1
    );
}

#[test]
fn restart_ignores_newline_terminated_batch_with_invalid_checksum() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "committed".into(),
            },
        )
        .unwrap();
    let snapshot = store.snapshot("shared").unwrap();
    let next_seq = snapshot.revision + 1;
    let run_id = RunId(uuid::Uuid::new_v4());
    let result = BrainEvent {
        schema_version: BRAIN_EVENT_SCHEMA_VERSION,
        brain_id: snapshot.brain_id,
        seq: next_seq,
        environment_generation: snapshot.environment.generation,
        sender: "daemon".into(),
        created_ms: unix_millis(),
        run_id: Some(run_id),
        mutation: None,
        kind: BrainEventKind::Result {
            request_seq: 1,
            output: String::new(),
            error: Some("torn disconnect".into()),
            continuation_messages: Vec::new(),
            invocation_metadata: None,
        },
    };
    let terminal = BrainEvent {
        seq: next_seq + 1,
        kind: BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Failed,
            detail: Some("torn disconnect".into()),
        },
        ..result.clone()
    };
    let encoded = serde_json::to_vec(&BrainJournalRecord::EventBatch {
        event_count: Some(2),
        payload_sha256: Some("invalid-checksum".into()),
        events: vec![result, terminal],
    })
    .unwrap();
    OpenOptions::new()
        .append(true)
        .open(temp.path().join("shared/events.jsonl"))
        .unwrap()
        .write_all(&[encoded.as_slice(), b"\n"].concat())
        .unwrap();
    drop(store);

    let recovered = BrainStore::with_root("box.local", Some(temp.path().into()));
    let recovered_snapshot = recovered.snapshot("shared").unwrap();
    assert_eq!(recovered_snapshot.revision, snapshot.revision);
    assert!(recovered_snapshot
        .events
        .iter()
        .all(|event| event.run_id != Some(run_id)));
}

#[tokio::test]
async fn terminal_transition_and_archive_prune_run_publication_gates() {
    let store = BrainStore::with_root("box.local", None);
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let (_, run) = store
        .accept_speculative_run("shared", "alice", attachment.attachment_id, "one".into())
        .unwrap();
    drop(
        store
            .acquire_run_publication("shared", run.run_id)
            .await
            .unwrap(),
    );
    assert_eq!(store.run_publication_gates.read().unwrap().len(), 1);
    store
        .transition_run(
            "shared",
            "daemon",
            run.run_id,
            BrainRunStatus::Cancelled,
            None,
        )
        .unwrap();
    assert!(store.run_publication_gates.read().unwrap().is_empty());

    let (_, second) = store
        .accept_speculative_run("shared", "alice", attachment.attachment_id, "two".into())
        .unwrap();
    drop(
        store
            .acquire_run_publication("shared", second.run_id)
            .await
            .unwrap(),
    );
    store.archive("shared").unwrap();
    assert!(store.run_publication_gates.read().unwrap().is_empty());
}

#[test]
fn run_ancestry_is_validated_and_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let parent_request = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "parent".into(),
            },
        )
        .unwrap();
    let parent = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            parent_request.seq,
            attachment.attachment_id,
            BrainRunStatus::Running,
        )
        .unwrap();
    let child_request = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "child".into(),
            },
        )
        .unwrap();
    let child = store
        .start_run_with_parent(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            child_request.seq,
            attachment.attachment_id,
            BrainRunStatus::QueuedForEnvironment,
            Some(parent.run_id),
        )
        .unwrap();
    assert_eq!(
        store
            .inspect_run("shared", child.run_id)
            .unwrap()
            .parent_run_id,
        Some(parent.run_id)
    );
    assert!(store
        .start_run_with_parent(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            child_request.seq,
            attachment.attachment_id,
            BrainRunStatus::QueuedForEnvironment,
            Some(RunId::new()),
        )
        .is_err());

    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(
        restarted
            .inspect_run("shared", child.run_id)
            .unwrap()
            .parent_run_id,
        Some(parent.run_id)
    );
    restarted
        .transition_run(
            "shared",
            "runner",
            parent.run_id,
            BrainRunStatus::Running,
            None,
        )
        .unwrap();
    restarted
        .transition_run(
            "shared",
            "runner",
            parent.run_id,
            BrainRunStatus::Completed,
            None,
        )
        .unwrap();
    assert!(restarted
        .start_run_with_parent(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            child_request.seq,
            attachment.attachment_id,
            BrainRunStatus::QueuedForEnvironment,
            Some(parent.run_id),
        )
        .is_err());
}

#[test]
fn restart_interrupts_started_runs_without_replaying_queued_runs() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let running_request = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "started".into(),
            },
        )
        .unwrap();
    let running = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            running_request.seq,
            attachment.attachment_id,
            BrainRunStatus::Running,
        )
        .unwrap();
    let queued_request = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "queued".into(),
            },
        )
        .unwrap();
    let queued = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            queued_request.seq,
            attachment.attachment_id,
            BrainRunStatus::QueuedForEnvironment,
        )
        .unwrap();
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = restarted.snapshot("shared").unwrap();
    assert_eq!(
        snapshot
            .runs
            .iter()
            .find(|candidate| candidate.run_id == running.run_id)
            .unwrap()
            .status,
        BrainRunStatus::Interrupted
    );
    assert_eq!(
        snapshot
            .runs
            .iter()
            .find(|candidate| candidate.run_id == queued.run_id)
            .unwrap()
            .status,
        BrainRunStatus::QueuedForEnvironment
    );
    assert!(snapshot.events.iter().any(|event| {
        matches!(
            event.kind,
            BrainEventKind::RunStatusChanged {
                run_id,
                status: BrainRunStatus::Interrupted,
                ..
            } if run_id == running.run_id
        )
    }));
}

#[test]
fn program_stack_is_rebuilt_from_the_event_log() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("workstation.local", Some(temp.path().into()));
    let first = store
        .push(
            "finch",
            "alice",
            BrainEventKind::Program {
                language: ProgramLanguage::Forth,
                source: "2 3 +".into(),
            },
        )
        .unwrap();
    store
        .push(
            "finch",
            "bob",
            BrainEventKind::Prompt {
                text: "explain that".into(),
            },
        )
        .unwrap();

    let restarted = BrainStore::with_root("workstation.local", Some(temp.path().into()));
    let snapshot = restarted.snapshot("finch").unwrap();
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.program_stack.len(), 1);
    assert_eq!(snapshot.program_stack[0].seq, first.seq);
    assert_eq!(snapshot.environment.machine, "workstation.local");
    assert_eq!(snapshot.events[0].environment_generation, 1);
}

#[test]
fn tool_and_approval_lifecycle_is_rebuilt_from_the_event_log() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("workstation.local", Some(temp.path().into()));
    let brain_id = store.snapshot("finch").unwrap().brain_id;
    let audience = BrainApprovalAudience {
        brain_id,
        brain: "finch".into(),
        attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        subject: "alice".into(),
        role: AttachmentRole::Driver,
        environment_generation: 1,
    };
    store
        .push(
            "finch",
            "provider",
            BrainEventKind::ToolCall {
                request_seq: 1,
                tool_id: "tool-1".into(),
                name: "search_word".into(),
                input: serde_json::json!({"query": "fib"}),
            },
        )
        .unwrap();
    store
        .push(
            "finch",
            "runner",
            BrainEventKind::ApprovalRequested {
                request_seq: 1,
                approval_id: "tool-1".into(),
                approval_kind: "tool".into(),
                subject: "search_word".into(),
                audience: Some(audience.clone()),
                detail: serde_json::json!({"input": {"query": "fib"}}),
            },
        )
        .unwrap();
    store
        .push(
            "finch",
            "alice",
            BrainEventKind::ApprovalDecided {
                request_seq: 1,
                approval_id: "tool-1".into(),
                decision: serde_json::json!({"choice": "approve_once"}),
            },
        )
        .unwrap();
    store
        .push(
            "finch",
            "runner",
            BrainEventKind::ToolResult {
                request_seq: 1,
                tool_id: "tool-1".into(),
                output: "found".into(),
                is_error: false,
            },
        )
        .unwrap();

    let restarted = BrainStore::with_root("workstation.local", Some(temp.path().into()));
    let snapshot = restarted.snapshot("finch").unwrap();
    assert!(matches!(
        &snapshot.events[0].kind,
        BrainEventKind::ToolCall { tool_id, input, .. }
            if tool_id == "tool-1" && input["query"] == "fib"
    ));
    assert!(matches!(
        &snapshot.events[1].kind,
        BrainEventKind::ApprovalRequested {
            approval_id,
            subject,
            audience: Some(event_audience),
            ..
        }
            if approval_id == "tool-1"
                && subject == "search_word"
                && event_audience == &audience
    ));
    assert!(matches!(
        &snapshot.events[2].kind,
        BrainEventKind::ApprovalDecided { approval_id, decision, .. }
            if approval_id == "tool-1" && decision["choice"] == "approve_once"
    ));
    assert!(matches!(
        &snapshot.events[3].kind,
        BrainEventKind::ToolResult { tool_id, output, is_error: false, .. }
            if tool_id == "tool-1" && output == "found"
    ));
}

#[test]
fn legacy_approval_event_without_audience_still_deserializes() {
    let event: BrainEvent = serde_json::from_value(serde_json::json!({
        "schema_version": 6,
        "brain_id": uuid::Uuid::nil(),
        "seq": 1,
        "environment_generation": 1,
        "sender": "runner",
        "created_ms": 0,
        "kind": "approval_requested",
        "request_seq": 1,
        "approval_id": "approval-1",
        "approval_kind": "tool",
        "subject": "search_word",
        "detail": {}
    }))
    .unwrap();
    assert!(matches!(
        event.kind,
        BrainEventKind::ApprovalRequested { audience: None, .. }
    ));
}

#[test]
fn pop_is_an_event_and_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store
        .push(
            "brain",
            "alice",
            BrainEventKind::Program {
                language: ProgramLanguage::Lisp,
                source: "(+ 1 2)".into(),
            },
        )
        .unwrap();
    let popped = store.pop_program("brain", "alice").unwrap().unwrap();
    assert!(matches!(popped.kind, BrainEventKind::ProgramPopped { .. }));

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert!(restarted
        .snapshot("brain")
        .unwrap()
        .program_stack
        .is_empty());
}

#[test]
fn subscribers_receive_the_authoritative_sequence() {
    let store = BrainStore::with_root("box.local", None);
    let mut first = store.subscribe("brain").unwrap();
    let mut second = store.subscribe("brain").unwrap();
    let event = store
        .push(
            "brain",
            "alice",
            BrainEventKind::Prompt { text: "hi".into() },
        )
        .unwrap();
    assert_eq!(first.try_recv().unwrap(), event);
    assert_eq!(second.try_recv().unwrap(), event);
}

#[test]
fn empty_named_brain_remains_listed_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let original = store.snapshot("quiet-brain").unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(restarted.list().unwrap(), vec!["quiet-brain"]);
    let restored = restarted.snapshot("quiet-brain").unwrap();
    assert_eq!(restored.revision, 0);
    assert_eq!(restored.brain_id, original.brain_id);
    assert_ne!(restored.brain_id, BrainId::nil());
    assert!(temp.path().join("quiet-brain/metadata.json").exists());
}

#[test]
fn initialization_contract_is_persisted_and_inert_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let before = store.initialization("quiet-brain").unwrap();
    let snapshot = store.snapshot("quiet-brain").unwrap();
    assert_eq!(before.brain_id, snapshot.brain_id);
    assert_eq!(before.capability_budget, crate::vm::EffectSet::pure());
    assert_eq!(snapshot.revision, 0);
    assert!(snapshot.runs.is_empty() && snapshot.schedules.is_empty());
    assert!(snapshot
        .events
        .iter()
        .all(|event| !matches!(event.kind, BrainEventKind::EffectRecorded { .. })));

    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(restarted.initialization("quiet-brain").unwrap(), before);
    assert_eq!(restarted.snapshot("quiet-brain").unwrap().revision, 0);
    assert!(temp.path().join("quiet-brain/initialization.json").exists());
}

#[test]
fn initialization_requires_an_explicit_scheduled_brain_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    let contract = store.initialization("shared").unwrap();
    let schedule = store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            10,
        )
        .unwrap();
    assert_eq!(schedule.source, contract.source);
    assert_eq!(schedule.grant_ceiling, contract.capability_budget);
    assert!(store.queue_due_schedules("shared", 9).unwrap().is_empty());
    let runs = store.queue_due_schedules("shared", 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].kind, BrainRunKind::Scheduled);
    assert_eq!(runs[0].status, BrainRunStatus::QueuedForEnvironment);

    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let reattached = restarted
        .attach(
            "shared",
            "alice",
            AttachmentRole::Driver,
            Some(attachment.attachment_id),
        )
        .unwrap();
    restarted
        .activate_connection(
            "shared",
            reattached.attachment_id,
            reattached.connection_id.unwrap(),
        )
        .unwrap();
    let same = restarted
        .schedule_initialization(
            "shared",
            reattached.attachment_id,
            reattached.connection_id.unwrap(),
            20,
        )
        .unwrap();
    assert_eq!(same.schedule_id, schedule.schedule_id);
    let snapshot = restarted.snapshot("shared").unwrap();
    assert_eq!(snapshot.schedules.len(), 1);
    assert_eq!(snapshot.runs.len(), 1);
    assert!(snapshot
        .events
        .iter()
        .all(|event| !matches!(event.kind, BrainEventKind::EffectRecorded { .. })));
}

#[test]
fn stale_connection_cannot_win_initialization_scheduling_race() {
    let store = BrainStore::with_root("box.local", None);
    let first = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let stale_connection = first.connection_id.unwrap();
    store
        .activate_connection("shared", first.attachment_id, stale_connection)
        .unwrap();
    store
        .detach("shared", first.attachment_id, stale_connection)
        .unwrap();
    let current = store
        .attach(
            "shared",
            "alice",
            AttachmentRole::Driver,
            Some(first.attachment_id),
        )
        .unwrap();
    let current_connection = current.connection_id.unwrap();
    store
        .activate_connection("shared", current.attachment_id, current_connection)
        .unwrap();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let stale_store = store.clone();
    let stale_barrier = barrier.clone();
    let attachment_id = current.attachment_id;
    let stale = std::thread::spawn(move || {
        stale_barrier.wait();
        stale_store.schedule_initialization("shared", attachment_id, stale_connection, 10)
    });
    let current_store = store.clone();
    let current_barrier = barrier.clone();
    let accepted = std::thread::spawn(move || {
        current_barrier.wait();
        current_store.schedule_initialization("shared", attachment_id, current_connection, 10)
    });
    barrier.wait();

    assert!(stale.join().unwrap().is_err());
    assert!(accepted.join().unwrap().is_ok());
    let snapshot = store.snapshot("shared").unwrap();
    assert_eq!(snapshot.schedules.len(), 1);
    assert_eq!(snapshot.schedules[0].created_by, "alice");
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|event| matches!(event.kind, BrainEventKind::ScheduleChanged { .. }))
            .count(),
        1
    );
}

#[test]
fn public_source_collision_cannot_claim_initialization_identity() {
    let store = BrainStore::with_root("box.local", None);
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    let contract = store.initialization("shared").unwrap();
    let active_ordinary = store
        .create_schedule(
            "shared",
            "alice",
            attachment.attachment_id,
            contract.language,
            contract.source.clone(),
            contract.capability_budget.clone(),
            50,
            None,
            BrainScheduleDeliveryPolicy::Coalesce,
        )
        .unwrap();
    let cancelled_ordinary = store
        .create_schedule(
            "shared",
            "alice",
            attachment.attachment_id,
            contract.language,
            contract.source.clone(),
            contract.capability_budget.clone(),
            60,
            None,
            BrainScheduleDeliveryPolicy::Coalesce,
        )
        .unwrap();
    assert!(store
        .cancel_schedule(
            "shared",
            "alice",
            attachment.attachment_id,
            cancelled_ordinary.schedule_id,
        )
        .unwrap());
    let delivered_ordinary = store
        .create_schedule(
            "shared",
            "alice",
            attachment.attachment_id,
            contract.language,
            contract.source.clone(),
            contract.capability_budget.clone(),
            5,
            None,
            BrainScheduleDeliveryPolicy::Coalesce,
        )
        .unwrap();
    assert_eq!(store.queue_due_schedules("shared", 5).unwrap().len(), 1);
    assert!(
        !store
            .inspect_schedule("shared", delivered_ordinary.schedule_id)
            .unwrap()
            .unwrap()
            .active
    );
    assert_eq!(active_ordinary.module_identity, None);
    assert_eq!(cancelled_ordinary.module_identity, None);
    assert_eq!(delivered_ordinary.module_identity, None);

    let initialization = store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            10,
        )
        .unwrap();
    assert_ne!(initialization.schedule_id, active_ordinary.schedule_id);
    assert_ne!(initialization.schedule_id, cancelled_ordinary.schedule_id);
    assert_ne!(initialization.schedule_id, delivered_ordinary.schedule_id);
    assert_eq!(
        initialization.module_identity,
        Some(contract.module_identity())
    );
    assert_eq!(store.snapshot("shared").unwrap().schedules.len(), 4);
}

#[test]
fn cancelled_or_failed_initialization_attempt_is_retried() {
    let store = BrainStore::with_root("box.local", None);
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    let cancelled = store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            10,
        )
        .unwrap();
    assert!(store
        .cancel_schedule(
            "shared",
            "alice",
            attachment.attachment_id,
            cancelled.schedule_id,
        )
        .unwrap());
    let retry = store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            20,
        )
        .unwrap();
    assert_ne!(retry.schedule_id, cancelled.schedule_id);

    let run = store.queue_due_schedules("shared", 20).unwrap().remove(0);
    store
        .transition_run(
            "shared",
            "daemon:runner",
            run.run_id,
            BrainRunStatus::Failed,
            Some("retryable initialization failure".into()),
        )
        .unwrap();
    let second_retry = store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            30,
        )
        .unwrap();
    assert_ne!(second_retry.schedule_id, retry.schedule_id);
    assert!(second_retry.active);
}

#[test]
fn interrupted_initialization_attempt_is_retried_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    let first = store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            10,
        )
        .unwrap();
    let run = store.queue_due_schedules("shared", 10).unwrap().remove(0);
    store
        .transition_run(
            "shared",
            "daemon:runner",
            run.run_id,
            BrainRunStatus::Running,
            None,
        )
        .unwrap();
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let reattached = restarted
        .attach(
            "shared",
            "alice",
            AttachmentRole::Driver,
            Some(attachment.attachment_id),
        )
        .unwrap();
    restarted
        .activate_connection(
            "shared",
            reattached.attachment_id,
            reattached.connection_id.unwrap(),
        )
        .unwrap();
    assert_eq!(
        restarted.inspect_run("shared", run.run_id).unwrap().status,
        BrainRunStatus::Interrupted
    );
    let retry = restarted
        .schedule_initialization(
            "shared",
            reattached.attachment_id,
            reattached.connection_id.unwrap(),
            20,
        )
        .unwrap();
    assert_ne!(retry.schedule_id, first.schedule_id);
    assert!(retry.active);
}

#[test]
fn scheduled_initialization_prevents_provisional_brain_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            10_000,
        )
        .unwrap();
    store
        .detach(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();

    assert!(!store.remove_if_unused("shared").unwrap());
    assert_eq!(store.snapshot("shared").unwrap().schedules.len(), 1);
    assert!(temp.path().join("shared/initialization.json").exists());
}

#[test]
fn tagged_initialization_schedule_must_match_reviewed_payload() {
    let store = BrainStore::with_root("box.local", None);
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    let mut schedule = store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            10,
        )
        .unwrap();
    schedule.source = "(define (unreviewed-startup) : int 2)".into();

    let error = store
        .push(
            "shared",
            "daemon",
            BrainEventKind::ScheduleChanged { schedule },
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("schedule digest does not match its source"));
}

#[test]
fn invalid_tagged_initialization_schedule_is_rejected_during_replay() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            10,
        )
        .unwrap();
    drop(store);

    let path = temp.path().join("shared/events.jsonl");
    let encoded = std::fs::read_to_string(&path).unwrap();
    let tampered = encoded.replace(
        DEFAULT_INITIALIZATION_SOURCE,
        "(define (unreviewed-startup) : int 2)",
    );
    assert_ne!(tampered, encoded);
    std::fs::write(&path, tampered).unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let error = restarted.snapshot("shared").unwrap_err();
    assert!(error
        .to_string()
        .contains("invalid reviewed-module schedule"));
}

#[test]
fn invalid_initialization_delivery_is_rejected_during_replay() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            10,
        )
        .unwrap();
    store.queue_due_schedules("shared", 10).unwrap();
    drop(store);

    let path = temp.path().join("shared/events.jsonl");
    let encoded = std::fs::read_to_string(&path).unwrap();
    let mut changed = false;
    let tampered = encoded
        .lines()
        .map(|line| {
            let mut event: serde_json::Value = serde_json::from_str(line).unwrap();
            if event["kind"] == "schedule_due" {
                event["due"]["source"] =
                    serde_json::Value::String("(define (unreviewed-delivery) : int 2)".into());
                changed = true;
            }
            serde_json::to_string(&event).unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(changed);
    std::fs::write(&path, format!("{tampered}\n")).unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let error = restarted.snapshot("shared").unwrap_err();
    assert!(error
        .to_string()
        .contains("invalid reviewed-module delivery"));
}

#[test]
fn delivered_or_completed_initialization_is_idempotent_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    let schedule = store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            10,
        )
        .unwrap();
    let run = store.queue_due_schedules("shared", 10).unwrap().remove(0);
    let delivered = store
        .schedule_initialization(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
            20,
        )
        .unwrap();
    assert_eq!(delivered.schedule_id, schedule.schedule_id);
    store
        .transition_run(
            "shared",
            "daemon:runner",
            run.run_id,
            BrainRunStatus::Running,
            None,
        )
        .unwrap();
    store
        .transition_run(
            "shared",
            "daemon:runner",
            run.run_id,
            BrainRunStatus::Completed,
            None,
        )
        .unwrap();
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let reattached = restarted
        .attach(
            "shared",
            "alice",
            AttachmentRole::Driver,
            Some(attachment.attachment_id),
        )
        .unwrap();
    restarted
        .activate_connection(
            "shared",
            reattached.attachment_id,
            reattached.connection_id.unwrap(),
        )
        .unwrap();
    let completed = restarted
        .schedule_initialization(
            "shared",
            reattached.attachment_id,
            reattached.connection_id.unwrap(),
            30,
        )
        .unwrap();
    assert_eq!(completed.schedule_id, schedule.schedule_id);
    assert_eq!(restarted.snapshot("shared").unwrap().schedules.len(), 1);
}

#[tokio::test]
async fn reviewed_initialization_module_is_typed_and_pure() {
    let store = BrainStore::with_root("box.local", None);
    let contract = store.initialization("reviewed").unwrap();
    let runtime = crate::runtime::ProgramRuntime::new();
    let outcome = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Lisp,
            source_id: Some(format!("brain-initialization:{}", contract.source_sha256)),
            source: contract.source,
            intent: "reviewed Brain initialization module".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(outcome.status, crate::runtime::ExecutionStatus::Completed);
    assert!(outcome.inferred_capabilities.is_empty());
    assert!(outcome.vm_side_effects.is_empty());
}

#[test]
fn unreviewed_persisted_initialization_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let mut contract = store.initialization("tampered").unwrap();
    contract.source = "(define (ambient-startup) : int 2)".into();
    contract.source_sha256 = hex::encode(Sha256::digest(contract.source.as_bytes()));
    std::fs::write(
        temp.path().join("tampered/initialization.json"),
        serde_json::to_vec_pretty(&contract).unwrap(),
    )
    .unwrap();
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert!(restarted
        .initialization("tampered")
        .unwrap_err()
        .to_string()
        .contains("not the reviewed built-in module"));
}

#[test]
fn unused_brain_is_removed_after_its_last_participant_leaves() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let generation = store.environment().generation;
    let attachment = store
        .attach("provisional", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let lease = store
        .acquire_runner_lease("provisional", "alice", generation, None, 60_000)
        .unwrap();

    store
        .detach(
            "provisional",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    assert!(!store.remove_if_unused("provisional").unwrap());
    store
        .release_runner_lease("provisional", lease.lease_id)
        .unwrap();
    assert!(store.remove_if_unused("provisional").unwrap());
    assert!(!temp.path().join("provisional").exists());
    assert!(!store.list().unwrap().contains(&"provisional".to_string()));
}

#[test]
fn pending_attachment_prevents_another_participant_from_removing_brain() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let first = store
        .attach("pending", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let second = store
        .attach("pending", "bob", AttachmentRole::Driver, None)
        .unwrap();

    store
        .detach("pending", first.attachment_id, first.connection_id.unwrap())
        .unwrap();
    assert!(!store.remove_if_unused("pending").unwrap());
    assert_eq!(
        store
            .snapshot("pending")
            .unwrap()
            .attachments
            .into_iter()
            .find(|attachment| attachment.attachment_id == second.attachment_id)
            .unwrap()
            .connection_id,
        second.connection_id
    );

    store
        .detach(
            "pending",
            second.attachment_id,
            second.connection_id.unwrap(),
        )
        .unwrap();
    assert!(store.remove_if_unused("pending").unwrap());
}

#[test]
fn substantive_brain_survives_after_every_participant_leaves() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("durable", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .push(
            "durable",
            "alice",
            BrainEventKind::Prompt {
                text: "remember this".into(),
            },
        )
        .unwrap();
    store
        .detach(
            "durable",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();

    assert!(!store.remove_if_unused("durable").unwrap());
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(
        restarted.snapshot("durable").unwrap().program_stack.len(),
        0
    );
    assert!(restarted
        .snapshot("durable")
        .unwrap()
        .events
        .iter()
        .any(|event| {
            matches!(&event.kind, BrainEventKind::Prompt { text } if text == "remember this")
        }));
}

#[test]
fn task_list_projection_survives_daemon_restart() {
    use crate::brain::tasks::{BrainTask, BrainTaskPriority, BrainTaskStatus};

    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let tasks = vec![BrainTask {
        id: "compile".into(),
        content: "Compile the candidate frontend".into(),
        status: BrainTaskStatus::InProgress,
        priority: BrainTaskPriority::High,
    }];
    store
        .push(
            "durable-tasks",
            "provider",
            BrainEventKind::TaskListReplaced {
                tasks: tasks.clone(),
            },
        )
        .unwrap();
    assert_eq!(store.snapshot("durable-tasks").unwrap().tasks, tasks);
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = restarted.snapshot("durable-tasks").unwrap();
    assert_eq!(snapshot.tasks, tasks);
    assert!(matches!(
        &snapshot.events.last().unwrap().kind,
        BrainEventKind::TaskListReplaced { tasks: restored } if restored == &tasks
    ));
}

#[test]
fn legacy_events_are_projected_into_the_persisted_brain_identity() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("legacy");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
            directory.join("events.jsonl"),
            r#"{"seq":1,"environment_generation":1,"sender":"alice","created_ms":0,"kind":"prompt","text":"hello"}
"#,
        )
        .unwrap();

    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = store.snapshot("legacy").unwrap();
    assert_ne!(snapshot.brain_id, BrainId::nil());
    assert_eq!(snapshot.events[0].schema_version, 1);
    assert_eq!(snapshot.events[0].brain_id, snapshot.brain_id);
    assert!(snapshot.events[0].mutation.is_none());

    let appended = store
        .push(
            "legacy",
            "bob",
            BrainEventKind::Prompt {
                text: "again".into(),
            },
        )
        .unwrap();
    assert_eq!(appended.schema_version, BRAIN_EVENT_SCHEMA_VERSION);
    assert_eq!(appended.brain_id, snapshot.brain_id);
}

#[test]
fn v12_and_v13_event_logs_restart_without_run_correlation() {
    for schema_version in [12, 13] {
        let temp = tempfile::tempdir().unwrap();
        let name = format!("legacy-v{schema_version}");
        let directory = temp.path().join(&name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
                directory.join("events.jsonl"),
                format!(
                    "{{\"schema_version\":{schema_version},\"brain_id\":\"00000000-0000-0000-0000-000000000000\",\"seq\":1,\"environment_generation\":1,\"sender\":\"alice\",\"created_ms\":0,\"kind\":\"prompt\",\"text\":\"hello\"}}\n"
                ),
            )
            .unwrap();

        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = store.snapshot(&name).unwrap();
        assert_eq!(snapshot.events[0].schema_version, schema_version);
        assert_eq!(snapshot.events[0].run_id, None);
    }
}

#[test]
fn correlated_run_started_json_roundtrip_uses_distinct_envelope_key() {
    let run_id = RunId::new();
    let run = BrainRun {
        run_id,
        kind: BrainRunKind::Speculative,
        parent_run_id: None,
        request_seq: 1,
        initiating_attachment_id: AttachmentId::new(),
        initiated_by: "alice".into(),
        status: BrainRunStatus::QueuedForEnvironment,
        started_ms: 1,
        updated_ms: 1,
        detail: None,
    };
    let event = BrainEvent {
        schema_version: BRAIN_EVENT_SCHEMA_VERSION,
        brain_id: BrainId::new(),
        seq: 2,
        environment_generation: 1,
        sender: "alice".into(),
        created_ms: 1,
        run_id: Some(run_id),
        mutation: None,
        kind: BrainEventKind::RunStarted { run },
    };

    let encoded = serde_json::to_string(&event).unwrap();
    assert!(encoded.contains("\"correlation_run_id\""));
    let decoded: BrainEvent = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, event);
}

#[test]
fn run_status_payload_id_remains_compatible_with_envelope_correlation() {
    let run_id = RunId::new();
    let legacy = serde_json::json!({
        "schema_version": 13,
        "brain_id": BrainId::nil(),
        "seq": 2,
        "environment_generation": 1,
        "sender": "daemon",
        "created_ms": 1,
        "kind": "run_status_changed",
        "run_id": run_id,
        "status": "cancelled"
    });
    let legacy: BrainEvent = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.run_id, None);
    assert!(matches!(
        legacy.kind,
        BrainEventKind::RunStatusChanged {
            run_id: payload_run_id,
            status: BrainRunStatus::Cancelled,
            ..
        } if payload_run_id == run_id
    ));

    let current = BrainEvent {
        schema_version: BRAIN_EVENT_SCHEMA_VERSION,
        brain_id: BrainId::new(),
        seq: 2,
        environment_generation: 1,
        sender: "daemon".into(),
        created_ms: 1,
        run_id: Some(run_id),
        mutation: None,
        kind: BrainEventKind::RunStatusChanged {
            run_id,
            status: BrainRunStatus::Cancelled,
            detail: None,
        },
    };
    let encoded = serde_json::to_string(&current).unwrap();
    let decoded: BrainEvent = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, current);
}

#[test]
fn schema_thirteen_without_mutation_receipts_remains_compatible() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let brain_id = store.snapshot("legacy-v13").unwrap().brain_id;
    drop(store);
    let mut event = journal_event(brain_id, 1, "legacy v13");
    event.schema_version = 13;
    std::fs::write(
        temp.path().join("legacy-v13/events.jsonl"),
        format!("{}\n", serde_json::to_string(&event).unwrap()),
    )
    .unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(restarted.snapshot("legacy-v13").unwrap().revision, 1);
}

#[test]
fn schema_thirteen_cannot_silently_carry_mutation_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = store.snapshot("legacy-receipt").unwrap();
    drop(store);
    let mut event = journal_event(snapshot.brain_id, 1, "invalid legacy receipt");
    event.schema_version = 13;
    event.mutation = Some(BrainMutationReceipt {
        mutation_id: uuid::Uuid::new_v4(),
        attachment_id: AttachmentId(uuid::Uuid::new_v4()),
        expected_revision: 0,
        environment_generation: snapshot.environment.generation,
        command_sha256: "sha256".into(),
    });
    std::fs::write(
        temp.path().join("legacy-receipt/events.jsonl"),
        format!("{}\n", serde_json::to_string(&event).unwrap()),
    )
    .unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let error = restarted.snapshot("legacy-receipt").unwrap_err();
    assert!(error
        .to_string()
        .contains("mutation receipt under legacy schema 13"));
}

#[test]
fn concurrent_metadata_creation_converges_on_one_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let workers = (0..8)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || {
                BrainStore::with_root("box.local", Some(root))
                    .snapshot("shared")
                    .unwrap()
                    .brain_id
            })
        })
        .collect::<Vec<_>>();
    let ids = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_ne!(ids[0], BrainId::nil());
}

#[test]
fn attachment_cursor_is_monotonic_and_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let attachment = store
        .activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    let head = store.snapshot("shared").unwrap().revision;
    let connection_id = attachment.connection_id.unwrap();
    let acknowledged = store
        .acknowledge("shared", attachment.attachment_id, connection_id, head)
        .unwrap();
    assert_eq!(acknowledged.acknowledged_seq, head);
    assert!(store
        .acknowledge("shared", attachment.attachment_id, connection_id, head + 1,)
        .is_err());
    assert!(store
        .acknowledge("shared", attachment.attachment_id, connection_id, head - 1,)
        .is_err());
    store
        .detach(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted
        .snapshot("shared")
        .unwrap()
        .attachments
        .into_iter()
        .find(|candidate| candidate.attachment_id == attachment.attachment_id)
        .unwrap();
    assert_eq!(restored.acknowledged_seq, head);
    assert!(!restored.connected);
    let reattached = restarted
        .attach(
            "shared",
            "alice",
            AttachmentRole::Driver,
            Some(attachment.attachment_id),
        )
        .unwrap();
    assert!(!reattached.connected);
    assert_eq!(reattached.acknowledged_seq, head);
    let reattached = restarted
        .activate_connection(
            "shared",
            reattached.attachment_id,
            reattached.connection_id.unwrap(),
        )
        .unwrap();
    assert!(reattached.connected);
    assert!(restarted
        .attach(
            "shared",
            "alice",
            AttachmentRole::Observer,
            Some(attachment.attachment_id),
        )
        .is_err());
}

#[test]
fn concurrent_attach_cannot_rebind_one_identity() {
    let store = Arc::new(BrainStore::with_root("box.local", None));
    let attachment_id = AttachmentId::new();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let attempts = [AttachmentRole::Driver, AttachmentRole::Observer]
        .into_iter()
        .map(|role| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                store.attach("shared", "alice", role, Some(attachment_id))
            })
        })
        .collect::<Vec<_>>();
    let results = attempts
        .into_iter()
        .map(|attempt| attempt.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    store
        .activate_connection(
            "shared",
            winner.attachment_id,
            winner.connection_id.unwrap(),
        )
        .unwrap();
    let snapshot = store.snapshot("shared").unwrap();
    assert_eq!(snapshot.attachments.len(), 1);
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|event| matches!(event.kind, BrainEventKind::ClientAttached { .. }))
            .count(),
        1
    );
}

#[test]
fn stale_connection_cannot_disconnect_a_reattached_client() {
    let store = BrainStore::with_root("box.local", None);
    let first = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .activate_connection("shared", first.attachment_id, first.connection_id.unwrap())
        .unwrap();
    store
        .detach("shared", first.attachment_id, first.connection_id.unwrap())
        .unwrap();
    let second = store
        .attach(
            "shared",
            "alice",
            AttachmentRole::Driver,
            Some(first.attachment_id),
        )
        .unwrap();
    store
        .activate_connection(
            "shared",
            second.attachment_id,
            second.connection_id.unwrap(),
        )
        .unwrap();

    assert_ne!(first.connection_id, second.connection_id);
    assert!(store
        .detach("shared", first.attachment_id, first.connection_id.unwrap(),)
        .is_err());
    let head = store.snapshot("shared").unwrap().revision;
    assert!(store
        .acknowledge(
            "shared",
            first.attachment_id,
            first.connection_id.unwrap(),
            head,
        )
        .is_err());
    assert!(store
        .require_connection(
            "shared",
            second.attachment_id,
            second.connection_id.unwrap(),
        )
        .is_ok());
}

#[test]
fn abandoned_attachment_reservation_expires_without_advancing_cursor_or_log() {
    let store = BrainStore::with_root("box.local", None);
    let first = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let first_connection = first.connection_id.unwrap();
    assert!(!first.connected);
    assert_eq!(store.snapshot("shared").unwrap().revision, 0);
    assert!(store
        .attach(
            "shared",
            "alice",
            AttachmentRole::Driver,
            Some(first.attachment_id),
        )
        .is_err());

    assert!(store
        .expire_pending_connection("shared", first.attachment_id, first_connection)
        .unwrap());
    let second = store
        .attach(
            "shared",
            "alice",
            AttachmentRole::Driver,
            Some(first.attachment_id),
        )
        .unwrap();
    assert_eq!(second.acknowledged_seq, 0);
    assert_eq!(store.snapshot("shared").unwrap().revision, 0);
    assert!(!store
        .expire_pending_connection("shared", first.attachment_id, first_connection)
        .unwrap());

    let active = store
        .activate_connection(
            "shared",
            second.attachment_id,
            second.connection_id.unwrap(),
        )
        .unwrap();
    assert!(active.connected);
    assert!(!store
        .expire_pending_connection(
            "shared",
            second.attachment_id,
            second.connection_id.unwrap(),
        )
        .unwrap());
    let snapshot = store.snapshot("shared").unwrap();
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.attachments[0].acknowledged_seq, 0);
}

#[test]
fn runner_lease_is_exclusive_renewable_and_event_sourced() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let generation = store.environment().generation;
    let lease = store
        .acquire_runner_lease("shared", "console-a", generation, None, 60_000)
        .unwrap();
    assert!(store
        .acquire_runner_lease("shared", "console-b", generation, None, 60_000)
        .is_err());
    assert!(store
        .acquire_runner_lease(
            "shared",
            "console-a",
            generation,
            Some(RunnerLeaseId(uuid::Uuid::new_v4())),
            60_000,
        )
        .is_err());
    let renewed = store
        .acquire_runner_lease(
            "shared",
            "console-a",
            generation,
            Some(lease.lease_id),
            120_000,
        )
        .unwrap();
    assert_eq!(renewed.lease_id, lease.lease_id);
    assert!(renewed.expires_ms >= lease.expires_ms);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(
        restarted
            .snapshot("shared")
            .unwrap()
            .runner_lease
            .unwrap()
            .lease_id,
        lease.lease_id
    );
    restarted
        .release_runner_lease("shared", lease.lease_id)
        .unwrap();
    assert!(restarted.snapshot("shared").unwrap().runner_lease.is_none());
    assert!(restarted
        .acquire_runner_lease("shared", "console-b", generation + 1, None, 60_000)
        .is_err());
    let replacement = restarted
        .acquire_runner_lease("shared", "console-b", generation, None, 60_000)
        .unwrap();
    assert!(restarted
        .expire_runner_lease("shared", replacement.lease_id, replacement.expires_ms - 1)
        .is_ok_and(|expired| !expired));
    assert!(restarted
        .expire_runner_lease("shared", replacement.lease_id, replacement.expires_ms)
        .unwrap());
    assert!(restarted.snapshot("shared").unwrap().runner_lease.is_none());
}

#[test]
fn runner_handoff_is_addressed_durable_and_atomically_replaces_the_lease() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let generation = store.environment().generation;
    let source = store
        .acquire_runner_lease("shared", "runner-a", generation, None, 60_000)
        .unwrap();
    let handoff = store
        .request_runner_handoff(
            "shared",
            "controller",
            "runner-b",
            source.lease_id,
            generation,
            30_000,
        )
        .unwrap();
    assert!(store
        .acquire_runner_lease("shared", "runner-b", generation, None, 60_000)
        .is_err());

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(
        restarted
            .snapshot("shared")
            .unwrap()
            .runner_handoff
            .as_ref()
            .unwrap()
            .handoff_id,
        handoff.handoff_id
    );
    assert!(restarted
        .accept_runner_handoff("shared", "runner-c", handoff.handoff_id, generation, 60_000,)
        .is_err());
    let replacement = restarted
        .accept_runner_handoff("shared", "runner-b", handoff.handoff_id, generation, 60_000)
        .unwrap();
    assert_ne!(replacement.lease_id, source.lease_id);
    let snapshot = restarted.snapshot("shared").unwrap();
    assert_eq!(snapshot.runner_lease.as_ref().unwrap(), &replacement);
    assert!(snapshot.runner_handoff.is_none());
    assert!(snapshot.runner_lease_was_handed_off(source.lease_id));
    assert!(!snapshot.runner_lease_was_handed_off(replacement.lease_id));
    assert!(snapshot.events.iter().any(|event| matches!(
        event.kind,
        BrainEventKind::RunnerHandoffCompleted { handoff_id, .. }
            if handoff_id == handoff.handoff_id
    )));
}

#[test]
fn handoff_replay_requires_exact_revision_and_environment_preconditions() {
    let mut store = BrainStore::with_root("box.local", None);
    let generation = store.environment().generation;
    let source = store
        .acquire_runner_lease("shared", "runner-a", generation, None, 60_000)
        .unwrap();
    let receipt = mutation_receipt(
        &store,
        AttachmentId::new(),
        uuid::Uuid::new_v4(),
        store.snapshot("shared").unwrap().revision,
        "request-handoff",
    );
    let handoff = store
        .request_runner_handoff_with_receipt(
            "shared",
            "controller",
            "runner-b",
            source.lease_id,
            generation,
            30_000,
            Some(receipt.clone()),
        )
        .unwrap();
    store.environment.generation += 1;
    assert_eq!(
        store
            .request_runner_handoff_with_receipt(
                "shared",
                "controller",
                "runner-b",
                source.lease_id,
                generation,
                30_000,
                Some(receipt.clone()),
            )
            .unwrap(),
        handoff
    );
    let mut changed_revision = receipt.clone();
    changed_revision.expected_revision += 1;
    assert!(store
        .request_runner_handoff_with_receipt(
            "shared",
            "controller",
            "runner-b",
            source.lease_id,
            generation,
            30_000,
            Some(changed_revision),
        )
        .unwrap_err()
        .to_string()
        .contains("different command or precondition"));
    let mut changed_environment = receipt;
    changed_environment.environment_generation += 1;
    assert!(store
        .request_runner_handoff_with_receipt(
            "shared",
            "controller",
            "runner-b",
            source.lease_id,
            generation,
            30_000,
            Some(changed_environment),
        )
        .unwrap_err()
        .to_string()
        .contains("different command or precondition"));
}

#[test]
fn releasing_or_cancelling_the_source_invalidates_a_runner_handoff() {
    let store = BrainStore::with_root("box.local", None);
    let generation = store.environment().generation;
    let source = store
        .acquire_runner_lease("shared", "runner-a", generation, None, 60_000)
        .unwrap();
    let first = store
        .request_runner_handoff(
            "shared",
            "controller",
            "runner-b",
            source.lease_id,
            generation,
            30_000,
        )
        .unwrap();
    store
        .cancel_runner_handoff("shared", first.handoff_id, "controller")
        .unwrap();
    assert!(store.snapshot("shared").unwrap().runner_handoff.is_none());

    let second = store
        .request_runner_handoff(
            "shared",
            "controller",
            "runner-b",
            source.lease_id,
            generation,
            30_000,
        )
        .unwrap();
    store
        .release_runner_lease("shared", source.lease_id)
        .unwrap();
    assert!(store.snapshot("shared").unwrap().runner_handoff.is_none());
    assert!(store
        .accept_runner_handoff("shared", "runner-b", second.handoff_id, generation, 60_000,)
        .is_err());
}

#[test]
fn cancelled_handoff_replays_success_after_response_loss_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let generation = store.environment().generation;
    let source = store
        .acquire_runner_lease("shared", "runner-a", generation, None, 60_000)
        .unwrap();
    let handoff = store
        .request_runner_handoff(
            "shared",
            "controller",
            "runner-b",
            source.lease_id,
            generation,
            30_000,
        )
        .unwrap();
    let receipt = mutation_receipt(
        &store,
        AttachmentId::new(),
        uuid::Uuid::new_v4(),
        store.snapshot("shared").unwrap().revision,
        "cancel-handoff",
    );
    store
        .cancel_runner_handoff_with_receipt(
            "shared",
            handoff.handoff_id,
            "controller",
            Some(receipt.clone()),
        )
        .unwrap();
    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    restarted
        .cancel_runner_handoff_with_receipt(
            "shared",
            handoff.handoff_id,
            "controller",
            Some(receipt),
        )
        .unwrap();
    assert!(restarted
        .snapshot("shared")
        .unwrap()
        .runner_handoff
        .is_none());
}

#[tokio::test]
async fn accepted_noop_mutations_consume_their_uuid_with_typed_outcomes() {
    let store = BrainStore::with_root("box.local", None);
    let attachment = AttachmentId::new();
    let schedule_id = ScheduleId::new();
    let schedule_receipt = mutation_receipt(
        &store,
        attachment,
        uuid::Uuid::new_v4(),
        0,
        "cancel-missing-schedule",
    );
    assert!(!store
        .cancel_schedule_with_receipt(
            "shared",
            "alice",
            attachment,
            schedule_id,
            Some(schedule_receipt.clone()),
        )
        .unwrap());
    assert!(!store
        .cancel_schedule_with_receipt(
            "shared",
            "alice",
            attachment,
            schedule_id,
            Some(schedule_receipt.clone()),
        )
        .unwrap());
    let mut changed = schedule_receipt;
    changed.expected_revision += 1;
    assert!(store
        .cancel_schedule_with_receipt("shared", "alice", attachment, schedule_id, Some(changed),)
        .is_err());

    let handoff_id = RunnerHandoffId::new();
    let handoff_receipt = mutation_receipt(
        &store,
        attachment,
        uuid::Uuid::new_v4(),
        1,
        "cancel-missing-handoff",
    );
    store
        .cancel_runner_handoff_with_receipt(
            "shared",
            handoff_id,
            "alice",
            Some(handoff_receipt.clone()),
        )
        .unwrap();
    store
        .cancel_runner_handoff_with_receipt("shared", handoff_id, "alice", Some(handoff_receipt))
        .unwrap();

    let accepted = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "queued".into(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            accepted.seq,
            attachment,
            BrainRunStatus::QueuedForEnvironment,
        )
        .unwrap();
    store
        .transition_run(
            "shared",
            "alice",
            run.run_id,
            BrainRunStatus::Cancelled,
            None,
        )
        .unwrap();
    let run_receipt = mutation_receipt(
        &store,
        attachment,
        uuid::Uuid::new_v4(),
        store.snapshot("shared").unwrap().revision,
        "cancel-run-again",
    );
    let reserved = store
        .reserve_run_cancellation(
            "shared",
            "alice",
            attachment,
            run.run_id,
            run_receipt.clone(),
        )
        .await
        .unwrap();
    assert!(!reserved.needs_runner_cancel);
    assert!(
        store
            .reserve_run_cancellation("shared", "alice", attachment, run.run_id, run_receipt,)
            .await
            .unwrap()
            .replayed
    );

    let missing_run = RunId::new();
    let missing_receipt = mutation_receipt(
        &store,
        attachment,
        uuid::Uuid::new_v4(),
        store.snapshot("shared").unwrap().revision,
        "cancel-missing-run",
    );
    assert!(store
        .reserve_run_cancellation(
            "shared",
            "alice",
            attachment,
            missing_run,
            missing_receipt.clone(),
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("does not exist"));
    assert!(store
        .reserve_run_cancellation(
            "shared",
            "alice",
            attachment,
            missing_run,
            missing_receipt.clone(),
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("does not exist or has already finished"));
    let mut changed = missing_receipt;
    changed.environment_generation += 1;
    assert!(store
        .reserve_run_cancellation("shared", "alice", attachment, missing_run, changed,)
        .await
        .unwrap_err()
        .to_string()
        .contains("different command or precondition"));
}

#[tokio::test]
async fn run_cancellation_recovers_each_persisted_crash_boundary() {
    for boundary in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = AttachmentId::new();
        let accepted = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "cancel me".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                accepted.seq,
                attachment,
                BrainRunStatus::Running,
            )
            .unwrap();
        let receipt = mutation_receipt(
            &store,
            attachment,
            uuid::Uuid::new_v4(),
            store.snapshot("shared").unwrap().revision,
            "cancel-run-crash",
        );
        store
            .reserve_run_cancellation("shared", "alice", attachment, run.run_id, receipt.clone())
            .await
            .unwrap();
        if boundary >= 1 {
            store
                .mark_run_cancellation_dispatching(
                    "shared",
                    "alice",
                    run.run_id,
                    receipt.mutation_id,
                )
                .unwrap();
        }
        if boundary >= 2 {
            store
                .mark_run_cancellation_reconciled(
                    "shared",
                    "alice",
                    run.run_id,
                    receipt.mutation_id,
                )
                .unwrap();
        }
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(
            restarted.inspect_run("shared", run.run_id).unwrap().status,
            BrainRunStatus::Cancelled
        );
        let replay = restarted
            .reserve_run_cancellation("shared", "alice", attachment, run.run_id, receipt.clone())
            .await
            .unwrap();
        assert!(replay.replayed);
        restarted
            .mark_run_cancellation_dispatching("shared", "alice", run.run_id, receipt.mutation_id)
            .unwrap();
        restarted
            .mark_run_cancellation_reconciled("shared", "alice", run.run_id, receipt.mutation_id)
            .unwrap();
        restarted
            .complete_reserved_run_cancellation("shared", "alice", run.run_id)
            .unwrap();
        assert_eq!(
            restarted.inspect_run("shared", run.run_id).unwrap().status,
            BrainRunStatus::Cancelled
        );
        assert!(
            !restarted
                .reserve_run_cancellation(
                    "shared",
                    "alice",
                    attachment,
                    run.run_id,
                    receipt.clone(),
                )
                .await
                .unwrap()
                .needs_runner_cancel
        );
        let mut conflicting = receipt.clone();
        conflicting.expected_revision += 1;
        assert!(restarted
            .reserve_run_cancellation("shared", "alice", attachment, run.run_id, conflicting,)
            .await
            .unwrap_err()
            .to_string()
            .contains("different command or precondition"));
        let conflicting_uuid = BrainMutationReceipt {
            mutation_id: uuid::Uuid::new_v4(),
            ..receipt
        };
        let error = restarted
            .reserve_run_cancellation("shared", "alice", attachment, run.run_id, conflicting_uuid)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("revision"), "{error:#}");
    }
}

#[tokio::test(start_paused = true)]
async fn reserved_cancellation_restart_retries_until_durable_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = AttachmentId::new();
    let accepted = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "cancel me".into(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            accepted.seq,
            attachment,
            BrainRunStatus::Running,
        )
        .unwrap();
    let receipt = mutation_receipt(
        &store,
        attachment,
        uuid::Uuid::new_v4(),
        store.snapshot("shared").unwrap().revision,
        "cancel-run-restart-retry",
    );
    store
        .reserve_run_cancellation("shared", "alice", attachment, run.run_id, receipt.clone())
        .await
        .unwrap();
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    restarted.fail_cancellation_terminal_appends_for_test(5);
    restarted.list().unwrap();
    assert_eq!(restarted.pending_disconnect_terminalization_retries(), 1);
    assert_eq!(
        restarted.inspect_run("shared", run.run_id).unwrap().status,
        BrainRunStatus::Running
    );
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            if restarted.inspect_run("shared", run.run_id).unwrap().status
                == BrainRunStatus::Cancelled
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(restarted.pending_disconnect_terminalization_retries(), 0);
    let snapshot = restarted.snapshot("shared").unwrap();
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|event| matches!(
                event.kind,
                BrainEventKind::RunStatusChanged { run_id: event_run_id, status, .. }
                    if event_run_id == run.run_id && status == BrainRunStatus::Cancelled
            ))
            .count(),
        1
    );
    assert!(!snapshot.events.iter().any(|event| {
        event.run_id == Some(run.run_id) && matches!(event.kind, BrainEventKind::Result { .. })
    }));
    assert!(
        restarted
            .reserve_run_cancellation("shared", "alice", attachment, run.run_id, receipt,)
            .await
            .unwrap()
            .replayed
    );
}

#[test]
fn runner_handoff_expiry_is_exact_and_durable() {
    let store = BrainStore::with_root("box.local", None);
    let generation = store.environment().generation;
    let source = store
        .acquire_runner_lease("shared", "runner-a", generation, None, 60_000)
        .unwrap();
    let handoff = store
        .request_runner_handoff(
            "shared",
            "controller",
            "runner-b",
            source.lease_id,
            generation,
            30_000,
        )
        .unwrap();
    assert!(!store
        .expire_runner_handoff("shared", handoff.handoff_id, handoff.expires_ms - 1)
        .unwrap());
    assert!(store
        .expire_runner_handoff("shared", handoff.handoff_id, handoff.expires_ms)
        .unwrap());
    assert!(store.snapshot("shared").unwrap().runner_handoff.is_none());
    assert!(!store
        .expire_runner_handoff("shared", handoff.handoff_id, handoff.expires_ms)
        .unwrap());
}

#[test]
fn archive_removes_a_brain_but_preserves_its_log() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brains");
    let store = BrainStore::with_root("box.local", Some(root.clone()));
    store
        .push("old", "alice", BrainEventKind::Prompt { text: "hi".into() })
        .unwrap();
    let retained_runtime = store.program_runtime("old").unwrap();
    retained_runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::AgentSpawn,
            selector: crate::vm::ResourceSelector::None,
        })
        .unwrap();

    let archive = store.archive("old").unwrap().unwrap();
    assert!(!store.list().unwrap().contains(&"old".to_string()));
    assert!(!root.join("old").exists());
    assert!(archive.join("events.jsonl").exists());
    assert!(archive.join("authority.json").exists());

    retained_runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::MemoryRead,
            selector: crate::vm::ResourceSelector::None,
        })
        .unwrap();
    assert!(
        !root.join("old").exists(),
        "a retained runtime must not recreate its archived authority path"
    );
}

#[tokio::test]
async fn attached_clients_share_one_ordered_turn_lane_per_brain() {
    let store = BrainStore::with_root("box.local", None);
    let first = store.execution_lock("brain").unwrap();
    let same_brain = store.execution_lock("brain").unwrap();
    let other_brain = store.execution_lock("other").unwrap();
    assert!(Arc::ptr_eq(&first, &same_brain));
    assert!(!Arc::ptr_eq(&first, &other_brain));

    let first_turn = first.lock_owned().await;
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let waiting = tokio::spawn(async move {
        let _second_turn = same_brain.lock_owned().await;
        entered_tx.send(()).unwrap();
    });
    tokio::task::yield_now().await;
    assert!(entered_rx.try_recv().is_err());

    drop(first_turn);
    entered_rx.recv().await.unwrap();
    waiting.await.unwrap();
}

#[tokio::test]
async fn named_brain_restores_one_typed_runtime_without_replaying_source() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let runtime = store.program_runtime("brain").unwrap();
    let outcome = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: Some("brain:event:1".into()),
            source: ": square ( S n:int -- S int ! pure ) n n * ;".into(),
            intent: "define square".into(),
            effect: crate::programs::ExecutionEffect::VmWrite,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(outcome.status, crate::runtime::ExecutionStatus::Completed);
    let committed_revision = outcome.output_revision;
    let committed = store
        .commit_runtime("brain", 1, outcome.output_revision, &runtime)
        .unwrap();
    let checkpoint_sha256 = match committed.kind {
        BrainEventKind::RuntimeCommitted {
            checkpoint_sha256, ..
        } => checkpoint_sha256,
        other => panic!("expected runtime checkpoint, found {other:?}"),
    };
    assert!(temp
        .path()
        .join("brain/runtime")
        .join(format!("{checkpoint_sha256}.capnp"))
        .is_file());
    assert!(!temp
        .path()
        .join("brain/runtime")
        .join(format!("{checkpoint_sha256}.json"))
        .exists());

    let event_log = std::fs::read_to_string(temp.path().join("brain/events.jsonl")).unwrap();
    for line in event_log.lines() {
        if let Err(error) = serde_json::from_str::<BrainEvent>(line) {
            panic!("checkpoint event must round-trip through JSONL: {error}\n{line}");
        }
    }

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.program_runtime("brain").unwrap();
    assert_eq!(restored.revision(), committed_revision);
    let outcome = restored
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Lisp,
            source_id: Some("brain:event:2".into()),
            source: "(square 7)".into(),
            intent: "call restored definition".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: restored.manifest_generation(),
            expected_revision: Some(restored.revision()),
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(outcome.status, crate::runtime::ExecutionStatus::Completed);
    assert_eq!(outcome.values, vec![crate::programs::ProgramValue::Int(49)]);
    assert_eq!(outcome.output_revision, committed_revision + 1);
}

#[tokio::test]
async fn named_brain_reads_legacy_json_checkpoint_without_rewriting_history() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store.snapshot("brain").unwrap();
    let runtime = crate::runtime::ProgramRuntime::new();
    let outcome = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Lisp,
            source_id: Some("legacy-checkpoint.lisp".into()),
            source: "(define (double (n : int)) (* n 2))".into(),
            intent: "create legacy checkpoint".into(),
            effect: crate::programs::ExecutionEffect::VmWrite,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await
        .unwrap();
    let checkpoint = runtime
        .revision_history()
        .unwrap()
        .into_iter()
        .find(|revision| revision.revision == outcome.output_revision)
        .and_then(|revision| revision.checkpoint)
        .unwrap();
    let encoded = serde_json::to_vec(&checkpoint).unwrap();
    let checkpoint_sha256 = hex::encode(Sha256::digest(&encoded));
    let runtime_directory = temp.path().join("brain/runtime");
    std::fs::create_dir_all(&runtime_directory).unwrap();
    std::fs::write(
        runtime_directory.join(format!("{checkpoint_sha256}.json")),
        encoded,
    )
    .unwrap();
    store
        .push(
            "brain",
            "legacy-daemon",
            BrainEventKind::RuntimeCommitted {
                request_seq: 1,
                runtime_revision: outcome.output_revision,
                checkpoint_sha256: checkpoint_sha256.clone(),
            },
        )
        .unwrap();
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.program_runtime("brain").unwrap();
    let called = restored
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: Some("legacy-checkpoint.forth".into()),
            source: "21 double".into(),
            intent: "restore legacy checkpoint".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: restored.manifest_generation(),
            expected_revision: Some(restored.revision()),
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(42)]);
    assert!(runtime_directory
        .join(format!("{checkpoint_sha256}.json"))
        .is_file());
    assert!(!runtime_directory
        .join(format!("{checkpoint_sha256}.capnp"))
        .exists());
}

#[tokio::test]
async fn named_brain_commits_a_validated_frontend_runner_checkpoint() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store.snapshot("brain").unwrap();
    let runner = crate::runtime::ProgramRuntime::new();
    let outcome = runner
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Lisp,
            source_id: Some("runner:event:1".into()),
            source: "(define (triple (n : int)) (* n 3))".into(),
            intent: "frontend runner definition".into(),
            effect: crate::programs::ExecutionEffect::VmWrite,
            declared_capabilities: Vec::new(),
            manifest_generation: runner.manifest_generation(),
            expected_revision: Some(runner.revision()),
            budget: None,
        })
        .await
        .unwrap();
    let checkpoint = runner
        .revision_history()
        .unwrap()
        .into_iter()
        .find(|revision| revision.revision == outcome.output_revision)
        .and_then(|revision| revision.checkpoint)
        .unwrap();
    store
        .commit_runner_runtime("brain", 1, outcome.output_revision, checkpoint)
        .unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.program_runtime("brain").unwrap();
    let called = restored
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: Some("test:restored-runner".into()),
            source: "14 triple".into(),
            intent: "call frontend definition after daemon restart".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: restored.manifest_generation(),
            expected_revision: Some(restored.revision()),
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(42)]);
}

#[tokio::test]
async fn frontend_replacement_reacquires_the_same_durable_brain() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let generation = store.environment().generation;
    let lease = store
        .acquire_runner_lease("dogfood", "frontend-a", generation, None, 60_000)
        .unwrap();
    let prompt = store
        .push(
            "dogfood",
            "developer",
            BrainEventKind::Prompt {
                text: "continue the self-upgrade goal".into(),
            },
        )
        .unwrap();
    let runner = crate::runtime::ProgramRuntime::new();
    let outcome = runner
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Lisp,
            source_id: Some("dogfood:define".into()),
            source: "(define (next-step (n : int)) (+ n 1))".into(),
            intent: "retain work across frontend replacement".into(),
            effect: crate::programs::ExecutionEffect::VmWrite,
            declared_capabilities: Vec::new(),
            manifest_generation: runner.manifest_generation(),
            expected_revision: Some(runner.revision()),
            budget: None,
        })
        .await
        .unwrap();
    let checkpoint = runner
        .revision_history()
        .unwrap()
        .into_iter()
        .find(|revision| revision.revision == outcome.output_revision)
        .and_then(|revision| revision.checkpoint)
        .unwrap();
    store
        .commit_runner_runtime("dogfood", prompt.seq, outcome.output_revision, checkpoint)
        .unwrap();
    store
        .release_runner_lease("dogfood", lease.lease_id)
        .unwrap();
    drop(store);

    // Model both halves of the production handoff: a daemon opens the
    // durable store again, then the replacement frontend acquires a fresh
    // lease for the same Brain identity.
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let replacement = restarted
        .acquire_runner_lease("dogfood", "frontend-a", generation, None, 60_000)
        .unwrap();
    let snapshot = restarted.snapshot("dogfood").unwrap();
    assert_eq!(
        snapshot.runner_lease.unwrap().lease_id,
        replacement.lease_id
    );
    assert!(snapshot.events.iter().any(|event| {
        matches!(
            &event.kind,
            BrainEventKind::Prompt { text }
                if text == "continue the self-upgrade goal"
        )
    }));

    let restored = restarted.program_runtime("dogfood").unwrap();
    let called = restored
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: Some("dogfood:resume".into()),
            source: "41 next-step".into(),
            intent: "resume after frontend replacement".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: restored.manifest_generation(),
            expected_revision: Some(restored.revision()),
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(42)]);
}

#[tokio::test]
async fn named_brain_restores_scoped_authority_from_its_separate_policy_record() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let runtime = store.program_runtime("brain").unwrap();
    let session_id = runtime.capability_session_id();
    let grant_id = runtime
        .issue_typed_capability(
            crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::AgentSpawn,
                selector: crate::vm::ResourceSelector::None,
            },
            crate::vm::GrantScope::Session { session_id },
            "test-user",
            None,
        )
        .unwrap();
    let outcome = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: Some("brain:event:1".into()),
            source: "42".into(),
            intent: "create a durable revision".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await
        .unwrap();
    store
        .commit_runtime("brain", 1, outcome.output_revision, &runtime)
        .unwrap();

    let authority_path = temp.path().join("brain/authority.json");
    assert!(authority_path.exists());
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.program_runtime("brain").unwrap();
    assert_eq!(restored.capability_session_id(), session_id);
    let ledger = restored.capability_ledger().unwrap();
    assert_eq!(ledger.grants.grants.len(), 1);
    assert_eq!(ledger.grants.grants[0].id, grant_id);
    assert!(matches!(
        ledger.grants.grants[0].scope,
        crate::vm::GrantScope::Session { session_id: restored_id } if restored_id == session_id
    ));
    assert_eq!(ledger.audit.len(), 1);
}

#[test]
fn named_brain_persists_grants_and_revocation_without_a_vm_commit() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let runtime = store.program_runtime("brain").unwrap();
    let grant_id = runtime
        .issue_typed_capability(
            crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::AgentSpawn,
                selector: crate::vm::ResourceSelector::None,
            },
            crate::vm::GrantScope::Session {
                session_id: runtime.capability_session_id(),
            },
            "test-user",
            None,
        )
        .unwrap();

    let after_grant = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = after_grant.program_runtime("brain").unwrap();
    assert_eq!(
        restored.capability_ledger().unwrap().grants.grants[0].id,
        grant_id
    );

    runtime.revoke_typed_capability(grant_id).unwrap();
    let after_revoke = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = after_revoke.program_runtime("brain").unwrap();
    let ledger = restored.capability_ledger().unwrap();
    assert!(ledger.grants.grants[0].revoked_at_unix_ms.is_some());
    assert_eq!(ledger.audit.len(), 2);
    assert!(matches!(
        ledger.audit[1].action,
        crate::vm::CapabilityAuditAction::Revoked
    ));
}

#[test]
fn named_brain_persists_policy_changes_and_denials_without_a_vm_commit() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let runtime = store.program_runtime("brain").unwrap();
    let requirement = crate::vm::CapabilityRequirement {
        capability: crate::vm::CapabilityKind::AgentSpawn,
        selector: crate::vm::ResourceSelector::None,
    };
    let grant_id = runtime
        .issue_typed_capability(
            requirement.clone(),
            crate::vm::GrantScope::Session {
                session_id: runtime.capability_session_id(),
            },
            "test-user",
            None,
        )
        .unwrap();
    let mut denied = std::collections::BTreeSet::new();
    denied.insert(crate::vm::CapabilityKind::AgentSpawn);
    assert_eq!(
        runtime
            .apply_capability_policy(
                crate::vm::CapabilityPolicy {
                    policy_hash: "locked-policy-v2".into(),
                    denied_capabilities: denied.clone(),
                },
                "policy-admin",
            )
            .unwrap(),
        vec![grant_id]
    );

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.program_runtime("brain").unwrap();
    assert_eq!(
        restored.capability_policy().unwrap(),
        crate::vm::CapabilityPolicy {
            policy_hash: "locked-policy-v2".into(),
            denied_capabilities: denied,
        }
    );
    assert!(restored
        .capability_ledger()
        .unwrap()
        .grants
        .grants
        .iter()
        .find(|grant| grant.id == grant_id)
        .unwrap()
        .revoked_at_unix_ms
        .is_some());
    assert!(restored
        .issue_typed_capability(
            requirement,
            crate::vm::GrantScope::Global,
            "test-user",
            None,
        )
        .unwrap_err()
        .to_string()
        .contains("denied by policy"));
}

#[tokio::test]
async fn named_brain_persists_denial_without_a_vm_commit() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let runtime = store.program_runtime("brain").unwrap();
    let pending = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Lisp,
            source_id: None,
            source: "(file-read (path \"Cargo.toml\"))".into(),
            intent: "test durable denial".into(),
            effect: crate::programs::ExecutionEffect::WorkspaceRead,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await
        .unwrap();
    let denied = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            crate::vm::ApprovalChoice::Deny,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(denied.status, crate::runtime::ExecutionStatus::Failed);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.program_runtime("brain").unwrap();
    assert!(matches!(
        restored
            .capability_ledger()
            .unwrap()
            .authorization_audit
            .last()
            .map(|entry| &entry.decision),
        Some(crate::vm::AuthorizationDecision::Denied { .. })
    ));
}

#[tokio::test]
async fn named_brain_persists_host_authorization_even_when_the_run_rolls_back() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let runtime = store.program_runtime("brain").unwrap();
    let grant_id = runtime
        .issue_typed_capability(
            crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
            ),
            crate::vm::GrantScope::Session {
                session_id: runtime.capability_session_id(),
            },
            "test-user",
            None,
        )
        .unwrap();
    let failed = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: None,
            source: "s\"Cargo.toml\" path file-read drop 1 0 /".into(),
            intent: "read then fail".into(),
            effect: crate::programs::ExecutionEffect::WorkspaceRead,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(failed.status, crate::runtime::ExecutionStatus::Failed);
    assert_eq!(runtime.revision(), 0, "failed VM state must roll back");

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let ledger = restarted
        .program_runtime("brain")
        .unwrap()
        .capability_ledger()
        .unwrap();
    assert!(matches!(
        ledger.authorization_audit.last().map(|entry| &entry.decision),
        Some(crate::vm::AuthorizationDecision::Allowed { grant_id: used }) if *used == grant_id
    ));
}

#[tokio::test]
async fn named_brain_checkpoint_without_authority_record_restores_without_grants() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let runtime = store.program_runtime("brain").unwrap();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::AgentSpawn,
            selector: crate::vm::ResourceSelector::None,
        })
        .unwrap();
    let outcome = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: None,
            source: "7".into(),
            intent: "checkpoint without authority".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await
        .unwrap();
    store
        .commit_runtime("brain", 1, outcome.output_revision, &runtime)
        .unwrap();
    std::fs::remove_file(temp.path().join("brain/authority.json")).unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.program_runtime("brain").unwrap();
    assert_eq!(restored.revision(), outcome.output_revision);
    assert!(restored
        .capability_ledger()
        .unwrap()
        .grants
        .grants
        .is_empty());
}

#[tokio::test]
async fn named_brain_rejects_a_tampered_authority_record() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let runtime = store.program_runtime("brain").unwrap();
    let outcome = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: None,
            source: "1".into(),
            intent: "persist authority envelope".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await
        .unwrap();
    store
        .commit_runtime("brain", 1, outcome.output_revision, &runtime)
        .unwrap();
    let authority_path = temp.path().join("brain/authority.json");
    let mut authority: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&authority_path).unwrap()).unwrap();
    authority["authority"]["project_id"] = serde_json::json!("tampered-project");
    std::fs::write(&authority_path, serde_json::to_vec(&authority).unwrap()).unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let error = restarted
        .program_runtime("brain")
        .err()
        .expect("tampered authority must fail closed");
    assert!(error.to_string().contains("restore authority"));
    assert!(format!("{error:#}").contains("integrity check"));
}

#[tokio::test]
async fn out_of_order_checkpoint_events_never_regress_a_brain_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let runtime = store.program_runtime("brain").unwrap();
    let submit = |source: &str, revision| crate::runtime::ProgramSubmission {
        language: crate::programs::ProgramLanguage::Forth,
        source_id: None,
        source: source.into(),
        intent: "concurrent checkpoint ordering".into(),
        effect: crate::programs::ExecutionEffect::Pure,
        declared_capabilities: Vec::new(),
        manifest_generation: runtime.manifest_generation(),
        expected_revision: Some(revision),
        budget: None,
    };
    let first = runtime.submit_typed_only(submit("1", 0)).await.unwrap();
    let second = runtime.submit_typed_only(submit("2", 1)).await.unwrap();
    store
        .commit_runtime("brain", 2, second.output_revision, &runtime)
        .unwrap();
    store
        .commit_runtime("brain", 1, first.output_revision, &runtime)
        .unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.program_runtime("brain").unwrap();
    assert_eq!(restored.revision(), second.output_revision);
    let values = restored
        .inspect()
        .await
        .unwrap()
        .stack
        .into_iter()
        .map(|cell| cell.value)
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        vec![
            crate::programs::ProgramValue::Int(1),
            crate::programs::ProgramValue::Int(2),
        ]
    );
}

#[tokio::test]
async fn legacy_restart_revision_reset_keeps_the_latest_request_state() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let runtime = crate::runtime::ProgramRuntime::new();
    let submit = |source: &str, revision| crate::runtime::ProgramSubmission {
        language: crate::programs::ProgramLanguage::Forth,
        source_id: None,
        source: source.into(),
        intent: "legacy revision migration".into(),
        effect: crate::programs::ExecutionEffect::Pure,
        declared_capabilities: Vec::new(),
        manifest_generation: runtime.manifest_generation(),
        expected_revision: Some(revision),
        budget: None,
    };
    let first = runtime
        .submit_typed_only(submit(": square ( S n:int -- S int ! pure ) n n * ;", 0))
        .await
        .unwrap();
    store
        .commit_runtime("brain", 1, first.output_revision, &runtime)
        .unwrap();
    let second = runtime
        .submit_typed_only(submit("1 drop", first.output_revision))
        .await
        .unwrap();
    store
        .commit_runtime("brain", 2, second.output_revision, &runtime)
        .unwrap();

    // ProgramRuntime::from_checkpoint historically reset its local
    // revision. Simulate an old daemon adding newer state as revision 1.
    let checkpoint = runtime
        .revision_history()
        .unwrap()
        .last()
        .and_then(|snapshot| snapshot.checkpoint.clone())
        .unwrap();
    let legacy_restarted = crate::runtime::ProgramRuntime::from_checkpoint(checkpoint).unwrap();
    let legacy_commit = legacy_restarted
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: None,
            source: ": cube ( S n:int -- S int ! pure ) n n * n * ;".into(),
            intent: "new state after legacy restart".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: legacy_restarted.manifest_generation(),
            expected_revision: Some(0),
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(legacy_commit.output_revision, 1);
    store
        .commit_runtime("brain", 3, legacy_commit.output_revision, &legacy_restarted)
        .unwrap();

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.program_runtime("brain").unwrap();
    assert_eq!(restored.revision(), 3);
    let called = restored
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Lisp,
            source_id: None,
            source: "(cube 4)".into(),
            intent: "call latest migrated definition".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: restored.manifest_generation(),
            expected_revision: Some(3),
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(64)]);
    assert_eq!(called.output_revision, 4);
}

#[test]
fn names_cannot_escape_the_storage_root() {
    assert!(BrainStore::validate_name("../other").is_err());
    assert!(BrainStore::validate_name("valid-brain_2").is_ok());
}

#[test]
// An environment is an indivisible authority boundary, not two routable heads.
fn environment_binds_machine_and_workspace_as_one_revision() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store =
        BrainStore::with_environment("gpu-box.local", workspace.path(), Some(state.path().into()));

    store
        .push(
            "project",
            "laptop.local",
            BrainEventKind::Prompt { text: "go".into() },
        )
        .unwrap();
    let snapshot = store.snapshot("project").unwrap();

    assert_eq!(snapshot.environment.machine, "gpu-box.local");
    assert_eq!(
        snapshot.environment.workspace,
        workspace.path().canonicalize().unwrap()
    );
    assert_eq!(snapshot.environment.generation, 1);
    assert_eq!(snapshot.events[0].environment_generation, 1);
}

#[test]
fn old_events_default_to_the_initial_environment_generation() {
    let event: BrainEvent = serde_json::from_str(
        r#"{"seq":1,"sender":"alice","created_ms":0,"kind":"prompt","text":"hi"}"#,
    )
    .unwrap();
    assert_eq!(event.environment_generation, 1);
}

fn audit_effect(sequence: u64, text: &str) -> crate::vm::VmSideEffect {
    crate::vm::VmSideEffect {
        protocol_version: 1,
        sequence,
        requirement: crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::SessionEmit,
            selector: crate::vm::ResourceSelector::None,
        },
        output: Vec::new(),
        event: crate::vm::HostSideEffect::Emit { text: text.into() },
        origin: crate::vm::SourceOrigin::generated("effect-audit-store-test"),
    }
}

fn audit_run_fixture(
    store: &BrainStore,
) -> (BrainRun, BrainRunnerLease, EffectAuditAuthorityGrant) {
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let prompt = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "effect".into(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            prompt.seq,
            attachment.attachment_id,
            BrainRunStatus::Running,
        )
        .unwrap();
    let lease = store
        .acquire_runner_lease("shared", "runner", 1, None, 300_000)
        .unwrap();
    let grant = store
        .issue_effect_audit_authority("shared", run.run_id, lease.lease_id, None)
        .unwrap();
    (run, lease, grant)
}

#[test]
fn effect_audit_permit_precedes_host_outcome_and_survives_turn_terminalization() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let (run, _lease, grant) = audit_run_fixture(&store);
    let identity = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "write"))
        .unwrap();
    let permit = store.begin_effect_audit(&grant, identity).unwrap();
    store
        .transition_run(
            "shared",
            "daemon",
            run.run_id,
            BrainRunStatus::Cancelled,
            Some("conversation cancelled".into()),
        )
        .unwrap();
    store
        .finish_effect_audit(
            &grant,
            Some(&permit),
            identity,
            crate::runtime::EffectAuditTerminalOutcome::Acknowledged {
                response: crate::runtime::VmResumeResponse::Result { values: Vec::new() },
            },
        )
        .unwrap();
    let snapshot = store.snapshot("shared").unwrap();
    assert!(matches!(snapshot.effect_audits[0].state,
            crate::runtime::EffectAuditState::Terminal {
                outcome: crate::runtime::EffectAuditTerminalOutcome::Redacted {
                    ref outcome_kind
                }
            } if outcome_kind == "acknowledged"));
    let fence = store
        .with_effect_audit_storage_mut("shared", snapshot.brain_id, |storage| {
            storage.replay.lookup(&identity)
        })
        .unwrap()
        .flatten()
        .unwrap();
    assert!(matches!(fence,
            crate::runtime::EffectAuditTransition::Fence {
                ref outcome_kind, ..
            } if outcome_kind == "acknowledged"));
    assert!(!snapshot
        .events
        .iter()
        .any(|event| matches!(event.kind, BrainEventKind::ToolResult { .. })));
}

#[test]
fn effect_audit_archive_exhaustion_fails_before_reserve_or_host_permit() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let (_run, _lease, grant) = audit_run_fixture(&store);
    store
        .exhaust_effect_audit_storage_for_test("shared")
        .unwrap();
    let error = store
        .reserve_effect_audit(
            &grant,
            uuid::Uuid::new_v4(),
            audit_effect(0, "must not execute"),
        )
        .unwrap_err();
    assert!(error.to_string().contains("storage exhausted"));
    assert!(
        store.snapshot("shared").unwrap().effect_audits.is_empty(),
        "storage exhaustion must fail before durable reserve or host permit"
    );
}

#[test]
fn effect_audit_begin_is_linear_and_never_reissues_a_physical_permit() {
    let store = BrainStore::with_root("box.local", None);
    let (_run, _lease, grant) = audit_run_fixture(&store);
    let identity = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "once"))
        .unwrap();
    let _permit = store.begin_effect_audit(&grant, identity).unwrap();
    assert!(store
        .begin_effect_audit(&grant, identity)
        .unwrap_err()
        .to_string()
        .contains("already begun"));
}

#[test]
fn effect_audit_observers_never_receive_secret_host_arguments() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let (_run, _lease, grant) = audit_run_fixture(&store);
    for (sequence, capability, secret) in [
        (0, crate::vm::CapabilityKind::FileWrite, "FILE_SECRET_163"),
        (
            1,
            crate::vm::CapabilityKind::NetworkConnect,
            "NETWORK_SECRET_163",
        ),
        (
            2,
            crate::vm::CapabilityKind::ProcessRun,
            "PROCESS_SECRET_163",
        ),
    ] {
        let effect = crate::vm::VmSideEffect {
            protocol_version: 1,
            sequence,
            requirement: crate::vm::CapabilityRequirement {
                capability,
                selector: crate::vm::ResourceSelector::None,
            },
            output: Vec::new(),
            event: crate::vm::HostSideEffect::Request {
                arguments: vec![crate::vm::TypedValue::String(secret.into())],
            },
            origin: crate::vm::SourceOrigin::generated(secret),
        };
        store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), effect)
            .unwrap();
    }
    let private_digests = store.brains.read().unwrap()["shared"]
        .effect_audits
        .entries()
        .values()
        .map(|entry| entry.intent.canonical_sha256.clone())
        .collect::<Vec<_>>();
    let snapshot = store.snapshot("shared").unwrap();
    let snapshot_json = serde_json::to_string(&snapshot).unwrap();
    let journal = std::fs::read_to_string(temp.path().join("shared/events.jsonl")).unwrap();
    for secret in [
        "FILE_SECRET_163",
        "NETWORK_SECRET_163",
        "PROCESS_SECRET_163",
    ] {
        assert!(!snapshot_json.contains(secret), "snapshot leaked {secret}");
        assert!(
            !journal.contains(secret),
            "canonical audit journal leaked {secret}"
        );
    }
    assert!(snapshot
        .effect_audits
        .iter()
        .all(|entry| entry.intent.canonical_sha256.is_empty()
            && entry.authority.authority_id == uuid::Uuid::nil()
            && entry.authority.runner_lease_id == uuid::Uuid::nil()));
    assert!(
        private_digests
            .iter()
            .all(|digest| !snapshot_json.contains(digest)),
        "ordinary snapshot leaked a private canonical replay digest"
    );
}

#[test]
fn effect_audit_reconnect_reuses_request_authority_and_exact_reservation() {
    let store = BrainStore::with_root("box.local", None);
    let (run, lease, first) = audit_run_fixture(&store);
    let second = store
        .issue_effect_audit_authority(
            "shared",
            run.run_id,
            lease.lease_id,
            Some(ConnectionId(uuid::Uuid::new_v4())),
        )
        .unwrap();
    assert_eq!(first.authority, second.authority);
    assert_eq!(first.authority.connection_id, None);
    let execution_id = uuid::Uuid::new_v4();
    let effect = audit_effect(0, "replayed reserve");
    let first_identity = store
        .reserve_effect_audit(&first, execution_id, effect.clone())
        .unwrap();
    let replayed_identity = store
        .reserve_effect_audit(&second, execution_id, effect)
        .unwrap();
    assert_eq!(first_identity, replayed_identity);
}

#[test]
fn effect_audit_request_end_reconciles_before_and_after_physical_boundary() {
    let store = BrainStore::with_root("box.local", None);
    let (_run, _lease, grant) = audit_run_fixture(&store);
    let unbegun = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "before"))
        .unwrap();
    let begun = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(1, "after"))
        .unwrap();
    let _permit = store.begin_effect_audit(&grant, begun).unwrap();
    assert_eq!(store.reconcile_effect_audit_authority(&grant).unwrap(), 2);
    assert_eq!(store.reconcile_effect_audit_authority(&grant).unwrap(), 0);
    let snapshot = store.snapshot("shared").unwrap();
    assert!(snapshot
        .effect_audits
        .iter()
        .any(|entry| entry.intent.identity == unbegun
            && matches!(
                entry.state,
                crate::runtime::EffectAuditState::Terminal {
                    outcome: crate::runtime::EffectAuditTerminalOutcome::AbandonedNotApplied
                }
            )));
    assert!(snapshot
        .effect_audits
        .iter()
        .any(|entry| entry.intent.identity == begun
            && matches!(
                entry.state,
                crate::runtime::EffectAuditState::Terminal {
                    outcome: crate::runtime::EffectAuditTerminalOutcome::UncertainProcessLoss
                }
            )));
}

#[test]
fn effect_audit_connection_teardown_batch_is_exact_idempotent_and_retryable() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let (_run, lease, grant) = audit_run_fixture(&store);
    let unbegun = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "unbegun"))
        .unwrap();
    let begun = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(1, "begun"))
        .unwrap();
    let _permit = store.begin_effect_audit(&grant, begun).unwrap();

    assert_eq!(
        store
            .reconcile_effect_audits_for_disconnected_leases(
                "shared",
                &[RunnerLeaseId(uuid::Uuid::new_v4())],
            )
            .unwrap(),
        0,
        "a different connection lease must not own these audit identities"
    );
    store
        .fail_next_effect_audit_batch_for_test("shared")
        .unwrap();
    assert!(store
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .is_err());
    let failed = store.snapshot("shared").unwrap();
    assert!(failed.effect_audits.iter().any(|entry| {
        entry.intent.identity == unbegun
            && matches!(
                entry.state,
                crate::runtime::EffectAuditState::IntentAccepted
            )
    }));
    assert!(failed.effect_audits.iter().any(|entry| {
        entry.intent.identity == begun
            && matches!(
                entry.state,
                crate::runtime::EffectAuditState::AwaitingHostResult
            )
    }));

    assert_eq!(
        store
            .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
            .unwrap(),
        0,
        "an exact teardown retry must not append duplicate terminal receipts"
    );
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = restarted.snapshot("shared").unwrap();
    assert!(snapshot
        .effect_audits
        .iter()
        .any(|entry| entry.intent.identity == unbegun
            && matches!(entry.state,
                    crate::runtime::EffectAuditState::Terminal {
                        outcome: crate::runtime::EffectAuditTerminalOutcome::Compacted {
                            ref outcome_kind, ..
                        }
                    } if outcome_kind == "abandoned_not_applied")));
    assert!(snapshot
        .effect_audits
        .iter()
        .any(|entry| entry.intent.identity == begun
            && matches!(entry.state,
                    crate::runtime::EffectAuditState::Terminal {
                        outcome: crate::runtime::EffectAuditTerminalOutcome::Compacted {
                            ref outcome_kind, ..
                        }
                    } if outcome_kind == "uncertain_process_loss")));
}

/// Absolute path of a Brain's active effect-audit journal under a store root.
fn active_journal_path(root: &std::path::Path) -> PathBuf {
    root.join("shared")
        .join("effect-audit-replay")
        .join("active.sqlite3")
}

/// Enough unresolved audits that the durable journal crosses at least one
/// SQLite page while they are reconciled. A two-row batch fits in already
/// allocated pages and never moves the file size, which would make every
/// byte-bound assertion below vacuous.
const AUDIT_BOUND_FIXTURE_AUDITS: usize = 64;

/// Build the shared effect-audit fixture on a real store root: a run, a
/// runner lease, and `AUDIT_BOUND_FIXTURE_AUDITS` durable reserves, all
/// through the production API.
fn audit_bound_fixture(
    root: &std::path::Path,
) -> (
    BrainStore,
    BrainRunnerLease,
    EffectAuditAuthorityGrant,
    Vec<crate::runtime::EffectAuditIdentity>,
) {
    let store = BrainStore::with_root("box.local", Some(root.to_path_buf()));
    let (_run, lease, grant) = audit_run_fixture(&store);
    let identities = (0..AUDIT_BOUND_FIXTURE_AUDITS as u64)
        .map(|sequence| {
            store
                .reserve_effect_audit(
                    &grant,
                    uuid::Uuid::new_v4(),
                    audit_effect(sequence, "bound"),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    (store, lease, grant, identities)
}

/// Measure, on a throwaway root, the journal size before the reconciliation
/// batch and the size that same batch durably reaches. `after` is the exact
/// size the batch's transaction commits at: the terminal fences that follow
/// delete rows but never shrink the SQLite file.
fn audit_batch_bound_probe() -> (u64, u64) {
    let temp = tempfile::tempdir().unwrap();
    let (store, lease, _grant, _identities) = audit_bound_fixture(temp.path());
    let before = store.effect_audit_journal_bytes_for_test("shared").unwrap();
    let reconciled = store
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .unwrap();
    let after = store.effect_audit_journal_bytes_for_test("shared").unwrap();
    assert_eq!(
        reconciled, AUDIT_BOUND_FIXTURE_AUDITS,
        "bound probe fixture must reconcile every audit under the production bound \
         (reconciled={reconciled} of {AUDIT_BOUND_FIXTURE_AUDITS}, \
          journal {before} -> {after} bytes)"
    );
    assert!(
        after > before,
        "bound probe is vacuous: the reconciliation batch no longer grows the active \
         effect-audit journal (before={before} bytes, after={after} bytes), so no bound \
         value distinguishes landing on the boundary from crossing it"
    );
    (before, after)
}

/// Find the single-transition `Begin` append that first grows the durable
/// journal, and measure the sizes it moves between. Returns the number of
/// begins that precede it so a regression can replay the same fixture and
/// place the bound exactly on that append.
fn audit_single_bound_probe() -> (usize, u64, u64) {
    let temp = tempfile::tempdir().unwrap();
    let (store, _lease, grant, identities) = audit_bound_fixture(temp.path());
    for (index, identity) in identities.iter().enumerate() {
        let before = store.effect_audit_journal_bytes_for_test("shared").unwrap();
        let _permit = store.begin_effect_audit(&grant, *identity).unwrap();
        let after = store.effect_audit_journal_bytes_for_test("shared").unwrap();
        if after > before {
            return (index, before, after);
        }
    }
    let settled = store.effect_audit_journal_bytes_for_test("shared").unwrap();
    panic!(
        "single-append bound probe is vacuous: no begin_effect_audit among \
         {AUDIT_BOUND_FIXTURE_AUDITS} audits grew the active effect-audit journal \
         (settled at {settled} bytes)"
    );
}

fn replayed_duplicate_seqs(events: &[BrainEvent]) -> Vec<(u64, usize)> {
    let mut seen = std::collections::BTreeMap::new();
    for event in events {
        *seen.entry(event.seq).or_insert(0usize) += 1;
    }
    seen.into_iter().filter(|(_, count)| *count > 1).collect()
}

/// #379 (effect-audit journal bound): a batch whose durable size lands exactly
/// on the byte bound is admitted, and the canonical revision advances once
/// per committed transition.
#[test]
fn test_effect_audit_batch_exactly_on_byte_bound_is_admitted_and_advances_revision() {
    let (probe_before, bound) = audit_batch_bound_probe();
    let temp = tempfile::tempdir().unwrap();
    let (store, lease, _grant, _identities) = audit_bound_fixture(temp.path());
    let before = store.effect_audit_journal_bytes_for_test("shared").unwrap();
    assert_eq!(
        before, probe_before,
        "effect-audit journal growth must be deterministic for the bound to be placeable: \
         probe measured {probe_before} bytes before the batch, this fixture {before}"
    );
    store
        .set_effect_audit_journal_max_bytes_for_test("shared", bound)
        .unwrap();
    let revision_before = store.snapshot("shared").unwrap().revision;

    let reconciled = store
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .unwrap();

    let after = store.effect_audit_journal_bytes_for_test("shared").unwrap();
    let revision_after = store.snapshot("shared").unwrap().revision;
    assert_eq!(
        reconciled, AUDIT_BOUND_FIXTURE_AUDITS,
        "a batch landing exactly on the durable byte bound must be admitted \
         (reconciled={reconciled} of {AUDIT_BOUND_FIXTURE_AUDITS}, bound={bound} bytes, \
          journal {before} -> {after} bytes, revision {revision_before} -> {revision_after})"
    );
    assert_eq!(
        after, bound,
        "the admitted batch must land exactly on the bound it was measured against \
         (bound={bound} bytes, journal {before} -> {after} bytes)"
    );
    assert_eq!(
        revision_after,
        revision_before + AUDIT_BOUND_FIXTURE_AUDITS as u64,
        "an admitted batch must advance the canonical revision once per transition \
         (revision {revision_before} -> {revision_after}, reconciled={reconciled}, \
          bound={bound} bytes, journal {before} -> {after} bytes)"
    );
}

/// #379 (effect-audit journal bound): past the durable byte bound the batch
/// must commit nothing. Before the fix the transaction committed and the
/// caller still saw `Err`, so the journal held transitions the canonical
/// revision did not know about.
#[test]
fn test_effect_audit_batch_past_byte_bound_commits_nothing_and_holds_the_sequence() {
    let (_probe_before, admitting_bound) = audit_batch_bound_probe();
    let bound = admitting_bound - 1;
    let temp = tempfile::tempdir().unwrap();
    let (store, lease, _grant, _identities) = audit_bound_fixture(temp.path());
    let journal_path = active_journal_path(temp.path());
    let bytes_before = std::fs::read(&journal_path).unwrap();
    let seqs_before = store.effect_audit_journal_seqs_for_test("shared").unwrap();
    let revision_before = store.snapshot("shared").unwrap().revision;
    store
        .set_effect_audit_journal_max_bytes_for_test("shared", bound)
        .unwrap();

    let refusal = store
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .expect_err(&format!(
            "a batch whose durable size would reach {admitting_bound} bytes must be refused \
             under a {bound}-byte bound"
        ));

    let bytes_after = std::fs::read(&journal_path).unwrap();
    let seqs_after = store.effect_audit_journal_seqs_for_test("shared").unwrap();
    let revision_after = store.snapshot("shared").unwrap().revision;
    assert!(
        format!("{refusal:#}").contains("durable byte bound"),
        "the refusal must name the durable byte bound so an operator can act on it \
         (bound={bound} bytes, error={refusal:#})"
    );
    assert!(
        bytes_before == bytes_after,
        "a refused effect-audit append must leave the durable journal byte-identical: \
         {} bytes before, {} bytes after, bound={bound}, refusal={refusal:#}",
        bytes_before.len(),
        bytes_after.len()
    );
    assert_eq!(
        seqs_after, seqs_before,
        "a refused effect-audit append must commit no sequence: journal held {seqs_before:?} \
         before and {seqs_after:?} after, bound={bound} bytes, refusal={refusal:#}"
    );
    assert_eq!(
        revision_after, revision_before,
        "a refused effect-audit append must leave the canonical revision unmoved \
         (revision {revision_before} -> {revision_after}, journal seqs {seqs_after:?}, \
          bound={bound} bytes, refusal={refusal:#})"
    );

    let ordinary = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "after refusal".into(),
            },
        )
        .unwrap();
    assert_eq!(
        ordinary.seq,
        revision_before + 1,
        "the next canonical append must take the seq the refused batch did not consume \
         (revision was {revision_before}, ordinary event took seq {}, journal holds \
          {seqs_after:?})",
        ordinary.seq
    );
    assert!(
        !seqs_after.contains(&ordinary.seq),
        "the ordinary canonical event took seq {} which the effect-audit journal already \
         holds ({seqs_after:?}) — this is the duplicate-seq corruption of #379 \
         (effect-audit journal bound)",
        ordinary.seq
    );
    store
        .set_effect_audit_journal_max_bytes_for_test(
            "shared",
            crate::brain::effect_audit_archive::MAX_ACTIVE_JOURNAL_BYTES,
        )
        .unwrap();
    let reconciled = store
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .unwrap();
    let revision_final = store.snapshot("shared").unwrap().revision;
    assert_eq!(
        reconciled, AUDIT_BOUND_FIXTURE_AUDITS,
        "the refused batch must still be retryable once the bound admits it \
         (reconciled={reconciled} of {AUDIT_BOUND_FIXTURE_AUDITS}, \
          revision {revision_after} -> {revision_final})"
    );
    assert_eq!(
        revision_final,
        ordinary.seq + AUDIT_BOUND_FIXTURE_AUDITS as u64,
        "the retried batch must allocate fresh seqs above the ordinary event \
         (ordinary seq {}, final revision {revision_final})",
        ordinary.seq
    );
    let retried = store
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .unwrap();
    assert_eq!(
        retried, 0,
        "a successful terminal batch must be exact-once: a second reconcile after \
         admission must append nothing (reconciled={retried}, revision {revision_final})"
    );
}

/// #379 (effect-audit journal bound): after a refused bound-crossing append
/// and an ordinary canonical append, a restart must still replay the Brain
/// with no duplicate `seq`.
#[test]
fn test_effect_audit_bound_refusal_does_not_reuse_a_seq_across_restart() {
    let (_probe_before, admitting_bound) = audit_batch_bound_probe();
    let bound = admitting_bound - 1;
    let temp = tempfile::tempdir().unwrap();
    let (store, lease, _grant, identities) = audit_bound_fixture(temp.path());
    let revision_before = store.snapshot("shared").unwrap().revision;
    store
        .set_effect_audit_journal_max_bytes_for_test("shared", bound)
        .unwrap();
    let refusal = store
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .expect_err("the bound-crossing reconciliation batch must be refused");
    store
        .set_effect_audit_journal_max_bytes_for_test(
            "shared",
            crate::brain::effect_audit_archive::MAX_ACTIVE_JOURNAL_BYTES,
        )
        .unwrap();
    let ordinary = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "after refusal".into(),
            },
        )
        .unwrap();
    let journal_seqs = store.effect_audit_journal_seqs_for_test("shared").unwrap();
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = restarted
        .snapshot("shared")
        .map_err(|error| {
            format!(
                "the Brain must still load after a refused bound-crossing append: {error:#} \
                 (revision before refusal {revision_before}, refusal={refusal:#}, ordinary event \
                  seq {}, effect-audit journal seqs {journal_seqs:?})",
                ordinary.seq
            )
        })
        .unwrap_or_else(|message| panic!("{message}"));
    let duplicates = replayed_duplicate_seqs(&snapshot.events);
    assert!(
        duplicates.is_empty(),
        "a refused bound-crossing append must not leave a duplicate canonical seq: \
         duplicates {duplicates:?} in replayed seqs {:?} (effect-audit journal held \
         {journal_seqs:?}, ordinary event took seq {}, revision before refusal \
         {revision_before})",
        snapshot
            .events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        ordinary.seq
    );
    let terminal = snapshot
        .effect_audits
        .iter()
        .filter(|entry| identities.contains(&entry.intent.identity) && entry.state.is_terminal())
        .count();
    assert_eq!(
        terminal,
        identities.len(),
        "restart replay must terminalize each refused identity exactly once \
         (terminal={terminal} of {}, ordinary seq {}, revision before refusal \
          {revision_before})",
        identities.len(),
        ordinary.seq
    );
    let retried = restarted
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .unwrap();
    assert_eq!(
        retried, 0,
        "restart replay must leave no post-terminal effect: a later reconcile must \
         append nothing (reconciled={retried}, terminal={terminal})"
    );
}

/// Crash after a bound refusal, before any retry or ordinary append. Restart
/// must load, drain unresolved identities exactly once, and not reuse a seq.
#[test]
fn test_effect_audit_bound_refusal_survives_crash_before_retry() {
    let (_probe_before, admitting_bound) = audit_batch_bound_probe();
    let bound = admitting_bound - 1;
    let temp = tempfile::tempdir().unwrap();
    let (store, lease, _grant, identities) = audit_bound_fixture(temp.path());
    let seqs_before = store.effect_audit_journal_seqs_for_test("shared").unwrap();
    let revision_before = store.snapshot("shared").unwrap().revision;
    store
        .set_effect_audit_journal_max_bytes_for_test("shared", bound)
        .unwrap();
    let refusal = store
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .expect_err("the bound-crossing reconciliation batch must be refused");
    let seqs_after = store.effect_audit_journal_seqs_for_test("shared").unwrap();
    assert_eq!(
        seqs_after, seqs_before,
        "a refused batch must commit no sequence before the crash \
         (seqs {seqs_before:?} -> {seqs_after:?}, revision {revision_before}, \
          refusal={refusal:#})"
    );
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = restarted
        .snapshot("shared")
        .map_err(|error| {
            format!(
                "the Brain must still load after a crash on a refused bound-crossing append: \
                 {error:#} (revision before refusal {revision_before}, refusal={refusal:#}, \
                  journal seqs before crash {seqs_after:?})"
            )
        })
        .unwrap_or_else(|message| panic!("{message}"));
    let duplicates = replayed_duplicate_seqs(&snapshot.events);
    assert!(
        duplicates.is_empty(),
        "a crash after bound refusal must not leave a duplicate canonical seq: \
         duplicates {duplicates:?} (revision before refusal {revision_before}, \
          journal seqs {seqs_after:?})"
    );
    let terminal = snapshot
        .effect_audits
        .iter()
        .filter(|entry| identities.contains(&entry.intent.identity) && entry.state.is_terminal())
        .count();
    assert_eq!(
        terminal,
        identities.len(),
        "restart after a refused bound-crossing append must terminalize each identity \
         exactly once (terminal={terminal} of {}, revision before refusal {revision_before})",
        identities.len()
    );
    let retried = restarted
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .unwrap();
    assert_eq!(
        retried, 0,
        "restart drain must be exact-once: a later reconcile must append nothing \
         (reconciled={retried}, terminal={terminal})"
    );
}

/// #379 (effect-audit journal bound): the single-transition `append` has the
/// identical shape and must refuse the same way. `begin_effect_audit` drives it.
#[test]
fn test_effect_audit_single_append_past_byte_bound_commits_nothing_and_holds_the_sequence() {
    let (preceding_begins, probe_before, admitting_bound) = audit_single_bound_probe();
    let bound = admitting_bound - 1;
    let temp = tempfile::tempdir().unwrap();
    let (store, _lease, grant, identities) = audit_bound_fixture(temp.path());
    for identity in &identities[..preceding_begins] {
        let _permit = store.begin_effect_audit(&grant, *identity).unwrap();
    }
    let identity = identities[preceding_begins];
    let journal_path = active_journal_path(temp.path());
    let bytes_before = std::fs::read(&journal_path).unwrap();
    assert_eq!(
        bytes_before.len() as u64,
        probe_before,
        "effect-audit journal growth must be deterministic for the bound to be placeable: \
         probe measured {probe_before} bytes before the begin append, this fixture {}",
        bytes_before.len()
    );
    let seqs_before = store.effect_audit_journal_seqs_for_test("shared").unwrap();
    let revision_before = store.snapshot("shared").unwrap().revision;
    store
        .set_effect_audit_journal_max_bytes_for_test("shared", bound)
        .unwrap();

    let refusal = store
        .begin_effect_audit(&grant, identity)
        .expect_err(&format!(
            "a begin append whose durable size would reach {admitting_bound} bytes must be \
             refused under a {bound}-byte bound"
        ));

    let bytes_after = std::fs::read(&journal_path).unwrap();
    let seqs_after = store.effect_audit_journal_seqs_for_test("shared").unwrap();
    let revision_after = store.snapshot("shared").unwrap().revision;
    assert!(
        bytes_before == bytes_after,
        "a refused single effect-audit append must leave the durable journal byte-identical: \
         {} bytes before, {} bytes after, bound={bound}, refusal={refusal:#}",
        bytes_before.len(),
        bytes_after.len()
    );
    assert_eq!(
        seqs_after, seqs_before,
        "a refused single effect-audit append must commit no sequence: journal held \
         {seqs_before:?} before and {seqs_after:?} after, bound={bound} bytes, \
         refusal={refusal:#}"
    );
    assert_eq!(
        revision_after, revision_before,
        "a refused single effect-audit append must leave the canonical revision unmoved \
         (revision {revision_before} -> {revision_after}, journal seqs {seqs_after:?}, \
          bound={bound} bytes, refusal={refusal:#})"
    );

    let ordinary = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "after refusal".into(),
            },
        )
        .unwrap();
    assert!(
        !seqs_after.contains(&ordinary.seq),
        "the ordinary canonical event took seq {} which the effect-audit journal already \
         holds ({seqs_after:?}) — this is the duplicate-seq corruption of #379 \
         (effect-audit journal bound)",
        ordinary.seq
    );
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = restarted
        .snapshot("shared")
        .map_err(|error| {
            format!(
                "the Brain must still load after a refused single bound-crossing append: \
                 {error:#} (ordinary event seq {}, effect-audit journal seqs {seqs_after:?}, \
                  revision before refusal {revision_before})",
                ordinary.seq
            )
        })
        .unwrap_or_else(|message| panic!("{message}"));
    let duplicates = replayed_duplicate_seqs(&snapshot.events);
    assert!(
        duplicates.is_empty(),
        "a refused single bound-crossing append must not leave a duplicate canonical seq: \
         duplicates {duplicates:?} in replayed seqs {:?} (effect-audit journal held \
         {seqs_after:?}, ordinary event took seq {})",
        snapshot
            .events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        ordinary.seq
    );
}

/// Late failure: inserts succeed, commit is aborted by the test hook, then an
/// ordinary canonical append and a restart must not reuse a seq, and retry
/// must terminalize exactly once.
#[test]
fn test_effect_audit_late_pre_commit_failure_holds_sequence_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let (_run, lease, grant) = audit_run_fixture(&store);
    let unbegun = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "unbegun"))
        .unwrap();
    let begun = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(1, "begun"))
        .unwrap();
    let _permit = store.begin_effect_audit(&grant, begun).unwrap();
    let seqs_before = store.effect_audit_journal_seqs_for_test("shared").unwrap();
    let revision_before = store.snapshot("shared").unwrap().revision;
    store
        .fail_next_effect_audit_batch_for_test("shared")
        .unwrap();
    let refusal = store
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .expect_err("injected pre-commit failure must abort the teardown batch");
    let seqs_after = store.effect_audit_journal_seqs_for_test("shared").unwrap();
    let revision_after = store.snapshot("shared").unwrap().revision;
    assert_eq!(
        seqs_after, seqs_before,
        "a late pre-commit failure must commit no sequence: journal held {seqs_before:?} \
         before and {seqs_after:?} after, refusal={refusal:#}"
    );
    assert_eq!(
        revision_after, revision_before,
        "a late pre-commit failure must leave the canonical revision unmoved \
         (revision {revision_before} -> {revision_after}, refusal={refusal:#})"
    );
    let ordinary = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "after late failure".into(),
            },
        )
        .unwrap();
    assert_eq!(
        ordinary.seq,
        revision_before + 1,
        "the next canonical append must take the seq the failed batch did not consume \
         (revision was {revision_before}, ordinary event took seq {})",
        ordinary.seq
    );
    assert!(
        !seqs_after.contains(&ordinary.seq),
        "the ordinary canonical event took seq {} which the effect-audit journal already \
         holds ({seqs_after:?})",
        ordinary.seq
    );
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = restarted
        .snapshot("shared")
        .map_err(|error| {
            format!(
                "the Brain must still load after a late pre-commit failure: {error:#} \
                 (ordinary event seq {}, journal seqs {seqs_after:?}, revision before \
                  failure {revision_before})",
                ordinary.seq
            )
        })
        .unwrap_or_else(|message| panic!("{message}"));
    let duplicates = replayed_duplicate_seqs(&snapshot.events);
    assert!(
        duplicates.is_empty(),
        "a late pre-commit failure must not leave a duplicate canonical seq: \
         duplicates {duplicates:?} (ordinary event seq {}, journal seqs {seqs_after:?})",
        ordinary.seq
    );
    let terminal = snapshot
        .effect_audits
        .iter()
        .filter(|entry| {
            (entry.intent.identity == unbegun || entry.intent.identity == begun)
                && entry.state.is_terminal()
        })
        .count();
    assert_eq!(
        terminal, 2,
        "restart after a late pre-commit failure must terminalize each identity exactly \
         once (terminal={terminal}, ordinary seq {})",
        ordinary.seq
    );
    let retried = restarted
        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
        .unwrap();
    assert_eq!(
        retried, 0,
        "restart drain must be exact-once: a later reconcile must append nothing \
         (reconciled={retried}, terminal={terminal})"
    );
}

#[test]
fn effect_audit_epochs_bound_active_history_and_fence_replay_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let (_run, _lease, grant) = audit_run_fixture(&store);
    let mut completed = Vec::new();
    for sequence in 0..3 {
        let execution_id = uuid::Uuid::new_v4();
        let effect = audit_effect(sequence, "terminal");
        let identity = store
            .reserve_effect_audit(&grant, execution_id, effect.clone())
            .unwrap();
        let permit = store.begin_effect_audit(&grant, identity).unwrap();
        store
            .finish_effect_audit(
                &grant,
                Some(&permit),
                identity,
                crate::runtime::EffectAuditTerminalOutcome::Acknowledged {
                    response: crate::runtime::VmResumeResponse::Result { values: Vec::new() },
                },
            )
            .unwrap();
        completed.push((identity, effect));
    }
    let unresolved = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(3, "unresolved"))
        .unwrap();
    let snapshot = store.snapshot("shared").unwrap();
    assert_eq!(
        snapshot.effect_audits.len(),
        4,
        "ordinary snapshots retain a bounded redacted terminal tail plus unresolved state"
    );
    assert!(snapshot
        .effect_audits
        .iter()
        .any(|entry| entry.intent.identity == unresolved && !entry.state.is_terminal()));
    let journal = std::fs::read(temp.path().join("shared/events.jsonl")).unwrap();
    assert!(
        !String::from_utf8_lossy(&journal).contains("terminal"),
        "detailed audit rows do not force transcript-history rewrites"
    );
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = restarted.snapshot("shared").unwrap();
    assert_eq!(
        snapshot.effect_audits.len(),
        4,
        "restart reconstructs the bounded redacted terminal observer tail"
    );
    let (identity, effect) = &completed[0];
    assert_eq!(
        restarted
            .reserve_effect_audit(&grant, identity.execution_id, effect.clone(),)
            .unwrap(),
        *identity,
        "exact lost reserve ACK replays before stale run/lease rejection"
    );
    let mut changed_effect = effect.clone();
    changed_effect.event = crate::vm::HostSideEffect::Emit {
        text: "changed secret".into(),
    };
    assert!(restarted
        .reserve_effect_audit(&grant, identity.execution_id, changed_effect,)
        .unwrap_err()
        .to_string()
        .contains("conflicting"));
    let mut forged = grant.clone();
    forged.authority.authority_id = uuid::Uuid::new_v4();
    assert!(restarted
        .reserve_effect_audit(&forged, identity.execution_id, effect.clone(),)
        .unwrap_err()
        .to_string()
        .contains("conflicting"));
}

#[test]
fn effect_audit_stale_successor_cannot_start_but_original_permit_can_finish() {
    let store = BrainStore::with_root("box.local", None);
    let (_run, lease, grant) = audit_run_fixture(&store);
    let identity = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "write"))
        .unwrap();
    let permit = store.begin_effect_audit(&grant, identity).unwrap();
    store
        .release_runner_lease("shared", lease.lease_id)
        .unwrap();
    store
        .acquire_runner_lease("shared", "successor", 1, None, 300_000)
        .unwrap();
    assert!(store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(1, "forged"),)
        .unwrap_err()
        .to_string()
        .contains("successor"));
    store
        .finish_effect_audit(
            &grant,
            Some(&permit),
            identity,
            crate::runtime::EffectAuditTerminalOutcome::FailedPartial {
                detail: "host reported a partial write".into(),
            },
        )
        .unwrap();
}

#[test]
fn effect_audit_restart_reconciles_without_reapplication() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let (_run, _lease, grant) = audit_run_fixture(&store);
    let unbegun = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "unbegun"))
        .unwrap();
    let begun = store
        .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(1, "begun"))
        .unwrap();
    let _permit = store.begin_effect_audit(&grant, begun).unwrap();
    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = restarted.snapshot("shared").unwrap();
    assert_eq!(snapshot.effect_audits.len(), 2);
    let unbegun_fence = restarted
        .with_effect_audit_storage_mut("shared", snapshot.brain_id, |storage| {
            storage.replay.lookup(&unbegun)
        })
        .unwrap()
        .flatten()
        .unwrap();
    let begun_fence = restarted
        .with_effect_audit_storage_mut("shared", snapshot.brain_id, |storage| {
            storage.replay.lookup(&begun)
        })
        .unwrap()
        .flatten()
        .unwrap();
    assert!(matches!(unbegun_fence,
            crate::runtime::EffectAuditTransition::Fence {
                ref outcome_kind, ..
            } if outcome_kind == "abandoned_not_applied"));
    assert!(matches!(begun_fence,
            crate::runtime::EffectAuditTransition::Fence {
                ref outcome_kind, ..
            } if outcome_kind == "uncertain_process_loss"));
}

#[test]
fn effect_audit_schema_v14_reconstructs_as_legacy_terminal_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let brain_id = store.snapshot("legacy-audit").unwrap().brain_id;
    drop(store);
    let event = BrainEvent {
        schema_version: 14,
        brain_id,
        seq: 1,
        environment_generation: 1,
        sender: "legacy-runner".into(),
        created_ms: 1,
        run_id: None,
        mutation: None,
        kind: BrainEventKind::EffectRecorded {
            request_seq: 1,
            execution_id: uuid::Uuid::new_v4(),
            effect: audit_effect(0, "legacy"),
            state: crate::vm::EffectJournalState::Acknowledged { values: Vec::new() },
        },
    };
    std::fs::write(
        temp.path().join("legacy-audit/events.jsonl"),
        format!("{}\n", serde_json::to_string(&event).unwrap()),
    )
    .unwrap();
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let snapshot = restarted.snapshot("legacy-audit").unwrap();
    assert_eq!(snapshot.effect_audits.len(), 1);
}

#[test]
fn future_effect_audit_schema_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let mut event = store
        .push(
            "future-audit",
            "alice",
            BrainEventKind::Prompt { text: "hi".into() },
        )
        .unwrap();
    drop(store);
    event.schema_version = BRAIN_EVENT_SCHEMA_VERSION + 1;
    std::fs::write(
        temp.path().join("future-audit/events.jsonl"),
        format!("{}\n", serde_json::to_string(&event).unwrap()),
    )
    .unwrap();
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert!(restarted
        .snapshot("future-audit")
        .unwrap_err()
        .to_string()
        .contains("unsupported event schema version"));
}

/// Concurrent archive and unused-delete must not resurrect the Brain and must
/// leave at most one terminal namespace outcome.
#[test]
fn test_concurrent_archive_and_remove_if_unused_do_not_resurrect() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    store
        .detach(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        )
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let archive_store = store.clone();
    let archive_barrier = barrier.clone();
    let archive = std::thread::spawn(move || {
        archive_barrier.wait();
        archive_store.archive("shared")
    });
    let remove_store = store.clone();
    let remove_barrier = barrier.clone();
    let remove = std::thread::spawn(move || {
        remove_barrier.wait();
        remove_store.remove_if_unused("shared")
    });
    barrier.wait();
    let archived = archive.join().unwrap();
    let _removed = remove.join().unwrap();
    let archive_absent_or_moved = match &archived {
        Ok(_) => true,
        Err(error) => error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
        }),
    };
    assert!(
        archive_absent_or_moved,
        "archive may move the log or lose the race to delete (not-found); it must not fail any other way; archive={archived:?}"
    );
    let names = store.list_names_unhydrated();
    assert!(
        !names.iter().any(|name| name == "shared"),
        "the active namespace must not contain the Brain after concurrent archive/delete; names={names:?}"
    );
    assert!(
        !temp.path().join("shared").exists(),
        "the live Brain directory must not remain after concurrent archive/delete; listing={}",
        directory_listing(temp.path())
    );
    let resurrected = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert!(
        !resurrected
            .list_names_unhydrated()
            .iter()
            .any(|name| name == "shared"),
        "a restarted store must not recreate an archived or deleted Brain; names={:?}",
        resurrected.list_names_unhydrated()
    );
}

/// A late completion after cancel must not change the terminal run.
#[test]
fn test_late_completion_after_cancel_leaves_run_cancelled() {
    let store = BrainStore::with_root("box.local", None);
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let prompt = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "late".into(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            prompt.seq,
            attachment.attachment_id,
            BrainRunStatus::QueuedForEnvironment,
        )
        .unwrap();
    store
        .transition_run("shared", "alice", run.run_id, BrainRunStatus::Running, None)
        .unwrap();
    let cancelled = store
        .transition_run(
            "shared",
            "alice",
            run.run_id,
            BrainRunStatus::Cancelled,
            Some("user cancel".into()),
        )
        .unwrap();
    let late = store.transition_run(
        "shared",
        "alice",
        run.run_id,
        BrainRunStatus::Completed,
        None,
    );
    assert!(
        late.is_err(),
        "late completion must be rejected; cancelled={cancelled:?} late={late:?}"
    );
    let inspected = store.inspect_run("shared", run.run_id).unwrap();
    assert_eq!(
        inspected.status,
        BrainRunStatus::Cancelled,
        "exact-once terminal state must remain Cancelled after a late completion; run={inspected:?}"
    );
}

/// A cancelled run stays cancelled across restart; a late Completed must not
/// append a second terminal event.
#[test]
fn test_late_completion_after_cancel_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let prompt = store
        .push(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "persist".into(),
            },
        )
        .unwrap();
    let run = store
        .start_run(
            "shared",
            "alice",
            BrainRunKind::Interactive,
            prompt.seq,
            attachment.attachment_id,
            BrainRunStatus::QueuedForEnvironment,
        )
        .unwrap();
    store
        .transition_run("shared", "alice", run.run_id, BrainRunStatus::Running, None)
        .unwrap();
    store
        .transition_run(
            "shared",
            "alice",
            run.run_id,
            BrainRunStatus::Cancelled,
            Some("user cancel".into()),
        )
        .unwrap();
    let late = store.transition_run(
        "shared",
        "alice",
        run.run_id,
        BrainRunStatus::Completed,
        None,
    );
    assert!(
        late.is_err(),
        "late completion must be rejected before restart; late={late:?}"
    );
    let terminal_events = |snapshot: &BrainSnapshot, run_id: RunId| {
        snapshot
            .events
            .iter()
            .filter(|event| {
                matches!(
                    &event.kind,
                    BrainEventKind::RunStatusChanged {
                        run_id: event_run,
                        status,
                        ..
                    } if *event_run == run_id && status.is_terminal()
                )
            })
            .count()
    };
    let before = store.snapshot("shared").unwrap();
    assert_eq!(
        terminal_events(&before, run.run_id),
        1,
        "cancel must publish exactly one terminal status event; events={:?}",
        before.events
    );
    drop(store);

    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let restored = restarted.inspect_run("shared", run.run_id).unwrap();
    assert_eq!(
        restored.status,
        BrainRunStatus::Cancelled,
        "restart replay must keep the cancelled terminal status; run={restored:?}"
    );
    let late_again = restarted.transition_run(
        "shared",
        "alice",
        run.run_id,
        BrainRunStatus::Completed,
        None,
    );
    assert!(
        late_again.is_err(),
        "late completion after restart must still be rejected; late={late_again:?}"
    );
    let after = restarted.snapshot("shared").unwrap();
    assert_eq!(
        after
            .runs
            .iter()
            .find(|candidate| candidate.run_id == run.run_id)
            .map(|candidate| candidate.status),
        Some(BrainRunStatus::Cancelled),
        "a late completion after restart must not revive the run; snapshot runs={:?}",
        after.runs
    );
    assert_eq!(
        terminal_events(&after, run.run_id),
        1,
        "restart plus late completion must not append a second terminal event; events={:?}",
        after.events
    );
}

/// Restart must replay an idempotent mutation exactly once.
#[test]
fn test_replay_mutation_is_stable_across_store_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let attachment = store
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let receipt = BrainMutationReceipt {
        mutation_id: uuid::Uuid::from_u128(7),
        attachment_id: attachment.attachment_id,
        expected_revision: store.snapshot("shared").unwrap().revision,
        environment_generation: store.environment().generation,
        command_sha256: "prompt".into(),
    };
    let first = store
        .push_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "hello".into(),
            },
            receipt.clone(),
        )
        .unwrap();
    let replayed = store
        .push_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "hello".into(),
            },
            receipt.clone(),
        )
        .unwrap();
    assert!(
        replayed.replayed && replayed.event.seq == first.event.seq,
        "in-process retry must return the original event; first={first:?} replayed={replayed:?}"
    );
    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    let after_restart = restarted.replay_mutation("shared", &receipt).unwrap();
    assert_eq!(
        after_restart.as_ref().map(|event| event.seq),
        Some(first.event.seq),
        "restart replay must resolve the same mutation without appending; after={after_restart:?}"
    );
}

fn delivery_envelope(
    execution_id: uuid::Uuid,
    sequence: u64,
    text: &str,
) -> crate::runtime::VmEffectEnvelope {
    crate::runtime::VmEffectEnvelope {
        execution_id,
        effect: crate::vm::VmSideEffect {
            protocol_version: 1,
            sequence,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            output: Vec::new(),
            event: crate::vm::HostSideEffect::Emit { text: text.into() },
            origin: crate::vm::SourceOrigin::generated("brain-delivery-test"),
        },
    }
}

fn output_handle_envelope(
    execution_id: uuid::Uuid,
    sequence: u64,
    handle: &str,
    generation: u64,
) -> crate::runtime::VmEffectEnvelope {
    crate::runtime::VmEffectEnvelope {
        execution_id,
        effect: crate::vm::VmSideEffect {
            protocol_version: 1,
            sequence,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            output: Vec::new(),
            event: crate::vm::HostSideEffect::Ui {
                operation: crate::vm::UiOperation::Status,
                target: Some(crate::vm::TypedValue::Resource {
                    kind: "output-handle".into(),
                    handle: handle.into(),
                    generation,
                }),
                text: Some(handle.into()),
                progress: None,
            },
            origin: crate::vm::SourceOrigin::generated("brain-delivery-handle"),
        },
    }
}

#[test]
fn brain_delivery_log_replays_unacked_suffix_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store.snapshot("shared").unwrap();
    let brain_id = store.snapshot("shared").unwrap().brain_id.0;
    let client = crate::runtime::DeliveryConsumerIdentity::new(brain_id, uuid::Uuid::new_v4());
    let execution_id = uuid::Uuid::new_v4();
    let first = delivery_envelope(execution_id, 0, "one");
    let second = delivery_envelope(execution_id, 1, "two");
    store
        .record_effect_delivery("shared", &[first.clone(), second.clone()])
        .unwrap();
    assert_eq!(
        store.pending_effect_delivery("shared", client).unwrap(),
        vec![first.clone(), second.clone()]
    );
    assert!(store
        .acknowledge_effect_delivery(
            "shared",
            client,
            crate::runtime::DeliveryCursor::through(execution_id, 0)
        )
        .unwrap());
    assert_eq!(
        store.pending_effect_delivery("shared", client).unwrap(),
        vec![second.clone()],
        "ack through sequence 0 must leave only the unacknowledged suffix"
    );

    drop(store);
    let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
    assert_eq!(
        restarted.pending_effect_delivery("shared", client).unwrap(),
        vec![second.clone()],
        "restart must replay the unacknowledged suffix"
    );
    let other = crate::runtime::DeliveryConsumerIdentity::new(brain_id, uuid::Uuid::new_v4());
    assert_eq!(
        restarted
            .pending_effect_delivery("shared", other)
            .unwrap()
            .len(),
        2,
        "a different client keeps an independent cursor"
    );
    assert!(restarted
        .acknowledge_effect_delivery(
            "shared",
            client,
            crate::runtime::DeliveryCursor::through(execution_id, 1)
        )
        .unwrap());
    assert!(restarted
        .pending_effect_delivery("shared", client)
        .unwrap()
        .is_empty());
}

#[test]
fn brain_delivery_log_indexes_concurrent_output_handles() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store.snapshot("shared").unwrap();
    let brain_id = store.snapshot("shared").unwrap().brain_id.0;
    let client = crate::runtime::DeliveryConsumerIdentity::new(brain_id, uuid::Uuid::new_v4());
    let execution_id = uuid::Uuid::new_v4();
    let download = output_handle_envelope(execution_id, 0, "download", 4);
    let log_handle = output_handle_envelope(execution_id, 1, "log", 1);
    store
        .record_effect_delivery("shared", &[download.clone(), log_handle.clone()])
        .unwrap();
    let log = store.effect_delivery_log("shared").unwrap().unwrap();
    let handles = log.lock().unwrap().output_handles(execution_id);
    assert_eq!(
        handles.len(),
        2,
        "concurrent output handles must be independently addressable; handles={handles:?}"
    );
    let download_ref = crate::runtime::OutputHandleRef::new(execution_id, "download", 4);
    assert_eq!(
        log.lock()
            .unwrap()
            .pending_for_handle(&client, &download_ref),
        vec![download]
    );
    let stale = crate::runtime::OutputHandleRef::new(execution_id, "download", 3);
    assert!(
        log.lock()
            .unwrap()
            .pending_for_handle(&client, &stale)
            .is_empty(),
        "a smaller generation must not match the live handle"
    );
}

#[tokio::test]
async fn program_runtime_from_brain_store_binds_the_delivery_log() {
    let temp = tempfile::tempdir().unwrap();
    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    store.snapshot("shared").unwrap();
    let runtime = store.program_runtime("shared").unwrap();
    assert!(
        runtime.effect_delivery_log().is_some(),
        "BrainStore::program_runtime must bind the production delivery log"
    );
    let outcome = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Lisp,
            source_id: Some("brain-delivery".into()),
            source: "(let ((handle (output-open \"download\"))) (output-complete handle))".into(),
            intent: "bind production delivery log".into(),
            effect: crate::programs::ExecutionEffect::VmRead,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(outcome.status, crate::runtime::ExecutionStatus::Completed);
    let brain_id = store.snapshot("shared").unwrap().brain_id.0;
    let client = crate::runtime::DeliveryConsumerIdentity::new(brain_id, uuid::Uuid::new_v4());
    let pending = store.pending_effect_delivery("shared", client).unwrap();
    assert!(
        pending
            .iter()
            .any(|envelope| envelope.execution_id == outcome.execution_id),
        "effects observed by the bound runtime must land in the Brain delivery log; pending={pending:?}"
    );
}

#[test]
fn archive_evicts_delivery_log_so_a_reused_name_does_not_leak() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brains");
    let store = BrainStore::with_root("box.local", Some(root.clone()));
    store.snapshot("shared").unwrap();
    let execution_id = uuid::Uuid::new_v4();
    let first = delivery_envelope(execution_id, 0, "archived");
    store
        .record_effect_delivery("shared", &[first.clone()])
        .unwrap();
    let archived = store.archive("shared").unwrap().unwrap();
    let archived_log = archived.join("runtime").join("effects.jsonl");
    let archived_bytes = std::fs::read(&archived_log).unwrap();
    assert!(
        !archived_bytes.is_empty(),
        "archived Brain must retain its delivery log"
    );

    store.snapshot("shared").unwrap();
    let new_id = store.snapshot("shared").unwrap().brain_id.0;
    let client = crate::runtime::DeliveryConsumerIdentity::new(new_id, uuid::Uuid::new_v4());
    assert!(
        store
            .pending_effect_delivery("shared", client)
            .unwrap()
            .is_empty(),
        "a reused name must open a new delivery log, not the archived Brain's"
    );
    let replacement = delivery_envelope(uuid::Uuid::new_v4(), 0, "replacement");
    store
        .record_effect_delivery("shared", &[replacement.clone()])
        .unwrap();
    assert_eq!(
        std::fs::read(&archived_log).unwrap(),
        archived_bytes,
        "recording on the reused name must not append into the archived effects.jsonl"
    );
    assert_eq!(
        store.pending_effect_delivery("shared", client).unwrap(),
        vec![replacement]
    );
}
