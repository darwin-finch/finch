//! The warm-up must log a Brain's failure once per failure, not once per pass
//! (#380).
//!
//! These assertions count real `tracing` output, which is the only form that
//! can fail: the first fix for #380 shipped with two tests over
//! `unloadable_brains`, a set that gates only the recovery `info`, and both
//! passed with the `warn` still firing at every 60 s re-warm.
//!
//! They live in their own test binary rather than in `src/brain/store.rs`
//! because `tracing` caches callsite interest process-globally. Sibling unit
//! tests call `warm_schedule_index` on other threads with no subscriber
//! installed, which races the thread-local capture and drops the event --
//! measured failing two runs in three under `--lib -- brain::store::`, and
//! passing under `--test-threads=1`. Owning the process removes the race
//! rather than papering over it; `rebuild_interest_cache` does not, because the
//! poisoning is concurrent, not merely earlier.

use std::sync::{Arc, Mutex};

use finch::brain::store::{
    AttachmentRole, BrainScheduleDeliveryPolicy, BrainStore, ProgramLanguage,
};
use finch::vm::EffectSet;
use tracing_subscriber::layer::SubscriberExt;

const WARM_FAILURE_LINE: &str = "could not be loaded while warming the schedule index";
const WARM_RECOVERY_LINE: &str = "became loadable again";

/// A `tracing` sink that keeps what was written so a test can count lines.
#[derive(Clone, Default)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl CapturedLog {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("log capture poisoned")).into_owned()
    }
}

impl std::io::Write for CapturedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("log capture poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn with_captured_logs<F: FnOnce()>(body: F) -> String {
    let sink = CapturedLog::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(move || writer.clone())
                .with_ansi(false),
        );
    tracing::subscriber::with_default(subscriber, body);
    sink.text()
}

fn count_lines_containing(log: &str, needle: &str) -> usize {
    log.lines().filter(|line| line.contains(needle)).count()
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

fn break_metadata(path: &std::path::Path) {
    std::fs::write(path, "{not json").expect("corrupt metadata");
}

#[test]
fn test_a_persistent_warm_failure_logs_one_warn_across_repeated_warms() {
    let temp = tempfile::tempdir().unwrap();
    {
        let seeding = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&seeding, "healthy", 1_000);
    }
    let broken = temp.path().join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    break_metadata(&broken.join("metadata.json"));

    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let log = with_captured_logs(|| {
        store.warm_schedule_index();
        store.warm_schedule_index();
        store.warm_schedule_index();
    });

    assert_eq!(
        count_lines_containing(&log, WARM_FAILURE_LINE),
        1,
        "three warms over one unchanging broken Brain must produce exactly one \
         warn -- a line per warm is unbounded growth describing a fact that \
         does not change until a human repairs it, and is what filled 30.4 MiB \
         with one identical line on the reference host. Captured log:\n{log}"
    );
    assert!(
        log.contains("brain=broken"),
        "the one line must name the Brain that failed, or an operator cannot \
         act on it. Captured log:\n{log}"
    );
}

/// Recovery is announced once, and suppression is per-Brain, not a global latch.
///
/// The second half guards against fixing the spam by simply never warning
/// twice: a Brain that breaks after another already did must still get its own
/// line. Note what this cannot cover -- a Brain that breaks *again* after being
/// repaired inside one process. `ensure_loaded` caches, so corrupting an
/// already-hydrated Brain's metadata is not observable until a restart; that
/// edge belongs to the restart path, and asserting it here would only be
/// asserting the cache.
#[test]
fn test_warm_logs_one_recovery_and_still_warns_for_a_later_distinct_failure() {
    let temp = tempfile::tempdir().unwrap();
    let good = {
        let seeding = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&seeding, "repairable", 2_000);
        std::fs::read(temp.path().join("repairable").join("metadata.json")).unwrap()
    };
    let metadata = temp.path().join("repairable").join("metadata.json");
    break_metadata(&metadata);

    let store = BrainStore::with_root("box.local", Some(temp.path().into()));
    let log = with_captured_logs(|| {
        store.warm_schedule_index();
        store.warm_schedule_index();
        std::fs::write(&metadata, &good).unwrap();
        store.warm_schedule_index();
        store.warm_schedule_index();

        let latecomer = temp.path().join("latecomer");
        std::fs::create_dir_all(&latecomer).unwrap();
        break_metadata(&latecomer.join("metadata.json"));
        store.warm_schedule_index();
        store.warm_schedule_index();
    });

    assert_eq!(
        count_lines_containing(&log, WARM_RECOVERY_LINE),
        1,
        "the repair must be announced once, not once per warm while the Brain \
         stays healthy. Captured log:\n{log}"
    );
    assert_eq!(
        count_lines_containing(&log, WARM_FAILURE_LINE),
        2,
        "two Brains failed, so two warns: suppression is keyed on the Brain, \
         not a process-wide 'already warned' latch that would hide every \
         failure after the first. Captured log:\n{log}"
    );
    assert!(
        log.contains("brain=latecomer"),
        "the second failure must name the Brain that actually failed. \
         Captured log:\n{log}"
    );
}
