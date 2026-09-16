//! Store-wide due index. Ordering is by due time, then Brain name, then id.

use std::collections::{HashMap, HashSet};

use super::{BrainSchedule, ScheduleId};

/// Every active schedule in the store, ordered by when it next comes due.
///
/// Schedule delivery used to select by *Brain*: the daemon enumerated the whole
/// Brain root once a second and hydrated every Brain to discover whether any of
/// them had work. Ordering by `next_due_ms` existed, but only inside a single
/// Brain and only after that Brain was already loaded. This is the same ordering
/// lifted to the store, so the daemon can select by *due time* and hydrate only
/// the Brains that actually have work (#374).
///
/// `due` is the ordering; `by_brain` exists so a single schedule can be moved
/// without scanning the map. The key carries the Brain name as well as the
/// schedule id: keying on `(next_due_ms, ScheduleId)` alone assumed schedule
/// ids are unique across Brains, and nothing enforces that at this boundary —
/// two Brains holding the same id at the same instant would silently overwrite
/// each other's entry, and the loser could never be repaired because
/// `by_brain` would still claim it was indexed.
type DueKey = (u64, String, ScheduleId);

#[derive(Debug, Default)]
pub struct ScheduleIndex {
    due: std::collections::BTreeMap<DueKey, ()>,
    by_brain: HashMap<String, HashMap<ScheduleId, u64>>,
    /// Brains whose *whole* schedule set has been read into the index, whether
    /// or not any of it is currently active.
    ///
    /// `by_brain` cannot answer this: `upsert` drops a Brain's entry the moment
    /// its last active slot goes, so a Brain that has one thousand spent
    /// one-shots and nothing active is absent from `by_brain` while being
    /// perfectly well indexed. `warm_schedule_index` gates its repair on this
    /// set so the gate is O(1) per Brain per warm; gating on `by_brain` would
    /// rescan every such Brain's entire lifetime schedule map, once a minute,
    /// forever — the growth curve `upsert`'s own doc comment exists to
    /// describe.
    indexed: HashSet<String>,
}

impl ScheduleIndex {
    /// Move or insert one schedule. O(log n), not O(the Brain's schedules).
    ///
    /// The whole-Brain rescan this replaced was O(every schedule the Brain had
    /// ever held): schedules are never pruned — deactivation sets a flag in
    /// place and one-shot schedules stay in the map forever — and the rescan
    /// ran inside the process-wide brains write guard, once per schedule event.
    pub fn upsert(&mut self, name: &str, schedule: &BrainSchedule) {
        let slots = self.by_brain.entry(name.to_string()).or_default();
        if let Some(previous) = slots.remove(&schedule.schedule_id) {
            self.due
                .remove(&(previous, name.to_string(), schedule.schedule_id));
        }
        if schedule.active {
            self.due.insert(
                (schedule.next_due_ms, name.to_string(), schedule.schedule_id),
                (),
            );
            slots.insert(schedule.schedule_id, schedule.next_due_ms);
        }
        if slots.is_empty() {
            self.by_brain.remove(name);
        }
    }

    /// Replace everything known about `name` with its current active schedules.
    ///
    /// Used where the whole set is the unit of work — a Brain becoming
    /// resident — rather than on the per-event path.
    pub fn reindex(&mut self, name: &str, schedules: &HashMap<ScheduleId, BrainSchedule>) {
        self.forget(name);
        for schedule in schedules.values().filter(|schedule| schedule.active) {
            self.upsert(name, schedule);
        }
        // After `forget`, so the Brain ends up marked known rather than
        // unknown. This is the only place a Brain becomes known: every other
        // mutation either moves a single schedule of an already-known Brain
        // (`upsert`) or makes it unknown again (`forget`).
        self.indexed.insert(name.to_string());
    }

    /// Forget a Brain entirely, for removal and archival.
    pub fn forget(&mut self, name: &str) {
        self.indexed.remove(name);
        if let Some(previous) = self.by_brain.remove(name) {
            for (schedule_id, next_due_ms) in previous {
                self.due
                    .remove(&(next_due_ms, name.to_string(), schedule_id));
            }
        }
    }

    /// Whether this Brain's schedule set has been read into the index.
    ///
    /// `false` means the index holds nothing for it *and* has not established
    /// that there is nothing to hold — the state a prune leaves behind, and the
    /// only state `warm_schedule_index` has to repair.
    pub fn is_indexed(&self, name: &str) -> bool {
        self.indexed.contains(name)
    }

    /// When the earliest active schedule in the store comes due.
    pub fn next_due_ms(&self) -> Option<u64> {
        self.due
            .keys()
            .next()
            .map(|(next_due_ms, _, _)| *next_due_ms)
    }

    /// Brains holding at least one schedule due at or before `now_ms`, in due
    /// order, each named once.
    pub fn due_brains(&self, now_ms: u64) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut brains = Vec::new();
        // The upper bound is the largest id at `now_ms`, so every schedule due at
        // exactly `now_ms` is included rather than dropped by an exclusive range.
        // Exclusive upper bound on the *next* millisecond rather than a
        // synthetic maximum key. `Brain` names are unbounded strings, so there
        // is no largest one to construct; `..(now_ms + 1, "", nil)` includes
        // every key whose instant is `now_ms` or earlier and excludes the rest,
        // whatever the name or id.
        let ceiling = (
            now_ms.saturating_add(1),
            String::new(),
            ScheduleId(uuid::Uuid::nil()),
        );
        for ((_, name, _), ()) in self.due.range(..ceiling) {
            if seen.insert(name.clone()) {
                brains.push(name.clone());
            }
        }
        brains
    }

    pub fn len(&self) -> usize {
        self.due.len()
    }

    /// Whether this Brain currently has at least one active indexed schedule.
    pub fn has_active(&self, name: &str) -> bool {
        self.by_brain.contains_key(name)
    }

    /// Diagnostic snapshot of every due key, never a rebuild.
    pub fn due_keys(&self) -> Vec<(u64, String, ScheduleId)> {
        self.due.keys().cloned().collect()
    }
}
