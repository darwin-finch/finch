//! The warm-up must log a Brain's failure once per failure, not once per pass
//! (#380 -- the daemon logged an unchanging condition on a timer).
//!
//! These assertions observe real `tracing` events, which is the only form that
//! can fail: the first fix for #380 shipped with two tests over
//! `unloadable_brains`, a set that gates only the recovery `info`, and both
//! passed with the `warn` still firing at every 60 s re-warm.
//!
//! They observe *structured* events -- level, target, and fields -- rather than
//! formatted text, because a substring count cannot tell a `warn` from an
//! `info` and cannot see whether the failure line still carries its reason.
//! #380 requires the line to name the Brain and the reason "so it stays
//! actionable", so both are asserted per event.
//!
//! They live in their own test binary because they are executable-level
//! regressions over real `tracing` output, which AGENTS.md places under
//! `tests/`; owning the process also means no sibling test shares the
//! process-global subscriber state these depend on.
//!
//! What is deliberately not covered here, and why:
//!
//! - A Brain that breaks *again* after being repaired inside one process.
//!   `ensure_loaded` caches, so corrupting an already-hydrated Brain's metadata
//!   is not observable until a restart. That edge belongs to the restart path,
//!   and asserting it here would only be asserting the cache.
//! - A Brain deleted *outside* the store and recreated under the same name with
//!   no warm-up in between. A Brain whose metadata cannot be parsed has no
//!   readable identity to record, so within one warm interval that sequence is
//!   indistinguishable from a repair. `archive` and `remove_if_unused` close
//!   the same window for the in-process paths by evicting eagerly; a bare
//!   `rm -rf` plus recreation inside one interval can still produce a single
//!   incorrect recovery line, and nothing in-process can tell the difference.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use finch::brain::store::{
    AttachmentRole, BrainScheduleDeliveryPolicy, BrainStore, ProgramLanguage,
};
use finch::vm::EffectSet;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

const WARM_FAILURE_LINE: &str = "could not be loaded while warming the schedule index";
const WARM_RECOVERY_LINE: &str = "became loadable again";

/// One `tracing` event, kept structurally so a test can assert on its level and
/// its fields and not merely on a substring of the rendered line.
#[derive(Clone, Debug)]
struct CapturedEvent {
    level: tracing::Level,
    target: String,
    fields: BTreeMap<String, String>,
}

impl CapturedEvent {
    fn message(&self) -> &str {
        self.field("message").unwrap_or("")
    }

    fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

impl std::fmt::Display for CapturedEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} {}:", self.level, self.target)?;
        for (name, value) in &self.fields {
            write!(formatter, " {name}={value}")?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct FieldVisitor(BTreeMap<String, String>);

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // `tracing`'s `%` sigil records through `record_debug` with a wrapper
        // whose `Debug` forwards to `Display`, so this yields the rendered
        // value without quoting it.
        self.0
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

#[derive(Clone, Default)]
struct CapturedEvents(Arc<Mutex<Vec<CapturedEvent>>>);

impl CapturedEvents {
    fn drain(&self) -> Vec<CapturedEvent> {
        self.0.lock().expect("captured events poisoned").clone()
    }
}

impl<S: tracing::Subscriber> Layer<S> for CapturedEvents {
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.0
            .lock()
            .expect("captured events poisoned")
            .push(CapturedEvent {
                level: *event.metadata().level(),
                target: event.metadata().target().to_string(),
                fields: visitor.0,
            });
    }
}

/// Capture every `finch` event emitted by `body`, at every level.
///
/// The filter is `trace` rather than `info` on purpose: a level downgrade must
/// fail the level assertion with the event in hand, not vanish through the
/// filter and be reported only as a missing line.
fn with_captured_events<F: FnOnce()>(body: F) -> Vec<CapturedEvent> {
    let captured = CapturedEvents::default();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("finch=trace"))
        .with(captured.clone());
    tracing::subscriber::with_default(subscriber, body);
    captured.drain()
}

fn warm_failures(events: &[CapturedEvent]) -> Vec<&CapturedEvent> {
    events
        .iter()
        .filter(|event| event.message().contains(WARM_FAILURE_LINE))
        .collect()
}

fn warm_recoveries(events: &[CapturedEvent]) -> Vec<&CapturedEvent> {
    events
        .iter()
        .filter(|event| event.message().contains(WARM_RECOVERY_LINE))
        .collect()
}

/// Render everything captured, so a failure explains itself without a re-run.
fn transcript(events: &[CapturedEvent]) -> String {
    if events.is_empty() {
        return "<no events captured>".to_string();
    }
    events
        .iter()
        .map(CapturedEvent::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// #380: the failure line must name the Brain and the reason, at warning level.
///
/// The level is asserted because a downgrade to `info` or `debug` is a silent
/// way to "fix" log volume by hiding the one line an operator needs. The reason
/// is asserted because without it the line names a Brain and nothing an
/// operator can act on, and because `%error` on an `anyhow::Error` renders only
/// the outermost context -- "parse .../metadata.json" -- unless it is rendered
/// with `{:#}`.
fn assert_failure_line_is_actionable(event: &CapturedEvent, brain: &str, events: &[CapturedEvent]) {
    assert_eq!(
        event.level,
        tracing::Level::WARN,
        "a Brain whose schedules will not be delivered until a human repairs \
         it must be reported at WARN; this event was {level}. Event: {event}\n\
         Captured:\n{captured}",
        level = event.level,
        captured = transcript(events)
    );
    assert_eq!(
        event.field("brain"),
        Some(brain),
        "the failure line must name the Brain that failed. Event: {event}\n\
         Captured:\n{captured}",
        captured = transcript(events)
    );
    let reason = event.field("error").unwrap_or_else(|| {
        panic!(
            "the failure line must carry the reason in an `error` field -- \
             #380 requires it to stay actionable, and a line that says only \
             which Brain failed is not. Event: {event}\nCaptured:\n{captured}",
            captured = transcript(events)
        )
    });
    assert!(
        !reason.trim().is_empty(),
        "the `error` field must carry a reason, not an empty string. \
         Event: {event}\nCaptured:\n{captured}",
        captured = transcript(events)
    );
    assert!(
        reason.contains("metadata.json"),
        "the reason must name what could not be read. Reason was {reason:?}. \
         Event: {event}\nCaptured:\n{captured}",
        captured = transcript(events)
    );
    assert!(
        reason.contains("line 1 column"),
        "the reason must carry the cause chain, not only the outermost \
         context: `parse .../metadata.json` tells an operator which file to \
         look at but not what is wrong with it, and `%error` on an \
         `anyhow::Error` renders exactly that much unless it is rendered with \
         `{{:#}}`. Reason was {reason:?}. Event: {event}\nCaptured:\n{captured}",
        captured = transcript(events)
    );
}

fn assert_recovery_line_names(event: &CapturedEvent, brain: &str, events: &[CapturedEvent]) {
    assert_eq!(
        event.level,
        tracing::Level::INFO,
        "a repair is ordinary good news and must not be reported at WARN or \
         above; this event was {level}. Event: {event}\nCaptured:\n{captured}",
        level = event.level,
        captured = transcript(events)
    );
    assert_eq!(
        event.field("brain"),
        Some(brain),
        "the recovery line must name the Brain that recovered, or an operator \
         cannot tell which repair worked. Event: {event}\nCaptured:\n{captured}",
        captured = transcript(events)
    );
}

/// Give `name` a schedule so the Brain is one the warm-up has reason to load.
fn seed_scheduled_brain(store: &BrainStore, name: &str, next_due_ms: u64) {
    let attachment = store
        .attach(name, "alice", AttachmentRole::Driver, None)
        .expect("attach");
    store
        .create_schedule(
            name,
            "alice",
            attachment.attachment_id,
            ProgramLanguage::Lisp,
            "(say \"tick\")",
            EffectSet::pure(),
            next_due_ms,
            Some(1_000),
            BrainScheduleDeliveryPolicy::Coalesce,
        )
        .expect("create schedule");
}

/// Break a Brain the way the reference host's Brain was broken: state on disk
/// the store cannot replay.
fn create_broken_brain(root: &std::path::Path, name: &str) {
    let directory = root.join(name);
    std::fs::create_dir_all(&directory).expect("create Brain directory");
    std::fs::write(directory.join("metadata.json"), "{not json").expect("corrupt metadata");
}

#[test]
fn test_a_persistent_warm_failure_logs_one_warn_across_repeated_warms() {
    let temp = tempfile::tempdir().unwrap();
    {
        let seeding = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&seeding, "healthy", 1_000);
    }
    create_broken_brain(temp.path(), "broken");

    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let events = with_captured_events(|| {
        store.warm_schedule_index();
        store.warm_schedule_index();
        store.warm_schedule_index();
    });

    let failures = warm_failures(&events);
    assert_eq!(
        failures.len(),
        1,
        "three warms over one unchanging broken Brain must produce exactly one \
         warn -- a line per warm is unbounded growth describing a fact that \
         does not change until a human repairs it, and is what filled 30.4 MiB \
         with one identical line on the reference host. Captured:\n{captured}",
        captured = transcript(&events)
    );
    assert_failure_line_is_actionable(failures[0], "broken", &events);
    assert!(
        warm_recoveries(&events).is_empty(),
        "nothing recovered, so nothing may be announced as recovered. \
         Captured:\n{captured}",
        captured = transcript(&events)
    );
}

/// Recovery is announced once, and suppression is per-Brain, not a global latch.
///
/// The second half guards against fixing the spam by simply never warning
/// twice: a Brain that breaks after another already did must still get its own
/// line.
#[test]
fn test_warm_logs_one_recovery_and_still_warns_for_a_later_distinct_failure() {
    let temp = tempfile::tempdir().unwrap();
    let good = {
        let seeding = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&seeding, "repairable", 2_000);
        std::fs::read(temp.path().join("repairable").join("metadata.json")).unwrap()
    };
    let metadata = temp.path().join("repairable").join("metadata.json");
    std::fs::write(&metadata, "{not json").unwrap();

    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let events = with_captured_events(|| {
        store.warm_schedule_index();
        store.warm_schedule_index();
        std::fs::write(&metadata, &good).unwrap();
        store.warm_schedule_index();
        store.warm_schedule_index();

        create_broken_brain(temp.path(), "latecomer");
        store.warm_schedule_index();
        store.warm_schedule_index();
    });

    let recoveries = warm_recoveries(&events);
    assert_eq!(
        recoveries.len(),
        1,
        "the repair must be announced once, not once per warm while the Brain \
         stays healthy. Captured:\n{captured}",
        captured = transcript(&events)
    );
    assert_recovery_line_names(recoveries[0], "repairable", &events);

    let failures = warm_failures(&events);
    assert_eq!(
        failures.len(),
        2,
        "two Brains failed, so two warns: suppression is keyed on the Brain, \
         not a process-wide 'already warned' latch that would hide every \
         failure after the first. Captured:\n{captured}",
        captured = transcript(&events)
    );
    assert_failure_line_is_actionable(failures[0], "repairable", &events);
    assert_failure_line_is_actionable(failures[1], "latecomer", &events);
}

/// Archiving a broken Brain and reusing its name must not fake a recovery.
///
/// Archiving is the natural remediation for a Brain the warm-up cannot load:
/// it needs no successful load, it is reachable live from the daemon's Brain
/// handlers, and it renames the directory out of the way. The failure the
/// warm-up recorded is keyed by *name*, and a name outlives the Brain that held
/// it, so a healthy Brain later created under that name would otherwise be
/// announced as `Brain <name> became loadable again; its schedules are
/// scheduled once more` -- false about the identity, since the BrainId is new,
/// and false about the schedules, which went with the archived directory.
#[test]
fn test_an_archived_brain_is_never_reported_as_recovered_when_its_name_is_reused() {
    let temp = tempfile::tempdir().unwrap();
    // A root below the tempdir, because `archive` writes `brains-archive`
    // beside the root and the tempdir must contain it.
    let root = temp.path().join("brains");
    std::fs::create_dir_all(&root).unwrap();
    {
        let seeding = BrainStore::with_root("box.local", Some(root.clone()));
        seed_scheduled_brain(&seeding, "healthy", 1_000);
    }
    create_broken_brain(&root, "reused");

    let store = BrainStore::with_root("box.local", Some(root.clone()));
    let events = with_captured_events(|| {
        store.warm_schedule_index();
        store
            .archive("reused")
            .expect("archive the unloadable Brain");
        // A different Brain, same name, healthy.
        seed_scheduled_brain(&store, "reused", 3_000);
        store.warm_schedule_index();
        store.warm_schedule_index();
    });

    assert!(
        warm_recoveries(&events).is_empty(),
        "the archived Brain did not become loadable again; a different Brain \
         now holds its name, with a new BrainId and none of its schedules. \
         Announcing that as a recovery makes two false claims in one line. \
         Captured:\n{captured}",
        captured = transcript(&events)
    );
    let failures = warm_failures(&events);
    assert_eq!(
        failures.len(),
        1,
        "only the archived Brain ever failed, and only once. \
         Captured:\n{captured}",
        captured = transcript(&events)
    );
    assert_failure_line_is_actionable(failures[0], "reused", &events);
}

/// A Brain removed outside the store is evicted, and its name starts fresh.
///
/// `rm -rf` on the directory is the remediation an operator reaches for first
/// and no store API observes it, so the registry is reconciled against the
/// warm-up's own enumeration. Two things must follow: the absence is not a
/// recovery, and the next Brain created under that name gets its own first
/// warning rather than inheriting its predecessor's silence -- which is the
/// single line #380 exists to guarantee.
#[test]
fn test_a_removed_brain_is_not_a_recovery_and_a_recreation_warns_fresh() {
    let temp = tempfile::tempdir().unwrap();
    {
        let seeding = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&seeding, "healthy", 1_000);
    }
    create_broken_brain(temp.path(), "gone");

    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let events = with_captured_events(|| {
        store.warm_schedule_index();
        std::fs::remove_dir_all(temp.path().join("gone")).unwrap();
        store.warm_schedule_index();
        store.warm_schedule_index();
        // A different Brain, same name, broken in its own right.
        create_broken_brain(temp.path(), "gone");
        store.warm_schedule_index();
        store.warm_schedule_index();
    });

    assert!(
        warm_recoveries(&events).is_empty(),
        "a Brain that was deleted has not been repaired. Reporting its absence \
         as `became loadable again; its schedules are scheduled once more` is \
         false about a directory that no longer exists. \
         Captured:\n{captured}",
        captured = transcript(&events)
    );
    let failures = warm_failures(&events);
    assert_eq!(
        failures.len(),
        2,
        "two distinct Brains failed under the same name, so two warns. A \
         registry that never forgets the name would suppress the second, which \
         is the first failure of a genuinely new Brain going unreported. \
         Captured:\n{captured}",
        captured = transcript(&events)
    );
    assert_failure_line_is_actionable(failures[0], "gone", &events);
    assert_failure_line_is_actionable(failures[1], "gone", &events);
}
