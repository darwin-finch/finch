//! How a memory read describes the index it read.
//!
//! Retrieval is deliberately ungated on hydration: a query runs against
//! whatever fraction of the MemTree has loaded. That is the right trade — a
//! partial answer beats a stalled prompt — but it is only honest if the answer
//! says so. Before this module, `hydration_status()` had no caller outside
//! `src/memory`, and every read surface reported a partial index as if it were
//! the whole store.
//!
//! The sharpest case is a read with no hits. "No relevant memories found" is a
//! claim about the *store*; the search only supports a claim about the *index
//! it read*. Against an index that never loaded, the two come apart, and a
//! model — which unlike a user has no status line to glance at — acts on the
//! sentence and concludes the memory was never recorded.
//!
//! Lives beside `src/memory` rather than inside it so the reporting vocabulary
//! is shared by the TUI status strip, the agent-facing memory tools, and the
//! typed runtime's `mem-recall` without any of them depending on each other.

use crate::memory::HydrationStatus;

/// How incomplete a status is, for picking the worse of two samples.
fn severity(status: &HydrationStatus) -> u8 {
    match status {
        HydrationStatus::Ready { .. } => 0,
        HydrationStatus::Loading { .. } => 1,
        HydrationStatus::Degraded { .. } => 2,
        HydrationStatus::Failed { .. } => 3,
    }
}

/// The status that describes what a query could actually have seen, given a
/// sample taken before it ran and one taken after.
///
/// Sampling only afterwards fails *open*, which is the dangerous direction: if
/// hydration finishes between the query and the sample, the status reads
/// `Ready` and the caller reports "no memories found" about a search that
/// covered a fraction of the store — reintroducing the exact false claim this
/// module exists to remove. Hydration also advances during the query itself
/// (it yields between batches, and `query_with_sources` contends for the same
/// locks), so an after-sample's `loaded` is a strict over-count of what was
/// searched.
///
/// Taking the worse of the two fails closed. When both are `Loading` the
/// earlier sample wins, because its count is a genuine lower bound on what the
/// search read — hence "at least" in the wording below.
pub(crate) fn observed(before: HydrationStatus, after: HydrationStatus) -> HydrationStatus {
    if severity(&after) > severity(&before) {
        after
    } else {
        before
    }
}

/// A sentence for a reader whose search did not cover the whole store, or
/// `None` when the index was complete and no qualification is owed.
///
/// `found_any` matters because `Failed` does not imply an empty index. It is
/// raised both when the first batch fails, where nothing loaded, and by the
/// hydration guard's `Drop`, which fires on panic, abort, or runtime shutdown
/// regardless of how much had already loaded. Hard-coding "nothing could be
/// searched" would print that under a list of results the search had just
/// returned — a self-contradiction, on a change whose whole point is not
/// making false claims.
pub(crate) fn caveat(status: &HydrationStatus, found_any: bool) -> Option<String> {
    match status {
        HydrationStatus::Ready { .. } => None,
        HydrationStatus::Loading { loaded, total } => Some(format!(
            "The memory index was still loading: at least {loaded} of {total} entries \
             had been read when this search ran. Retrying shortly may find more."
        )),
        // Deliberately not "the rest are unreadable". Hydration stops at the
        // first failing batch, so of the entries that did not load, only that
        // one batch is known bad and everything after it was never attempted.
        // Calling the remainder unreadable would assert something about data
        // that was not read -- structurally the same error as the absence claim
        // this wording replaces.
        HydrationStatus::Degraded { loaded, total, .. } => Some(format!(
            "The memory index is incomplete: {loaded} of {total} entries loaded before a \
             read error stopped it, and the remainder were not read. Retrying will not \
             find more."
        )),
        HydrationStatus::Failed { .. } if found_any => Some(
            "The memory index then failed: these results come from the part that had \
             loaded, and the rest of the store was not searched."
                .to_string(),
        ),
        HydrationStatus::Failed { .. } => {
            Some("The memory index is unavailable, so nothing could be searched.".to_string())
        }
    }
}

/// The TUI status-strip line: what was recalled, and out of how much.
///
/// `entries` rather than `memories` on purpose. The counts are `tree_nodes`
/// rows, which include the synthetic root and every internal aggregation node,
/// so they are not the number of things the user would call a memory.
pub(crate) fn status_line(recalled: usize, status: &HydrationStatus) -> String {
    match status {
        HydrationStatus::Ready { .. } => format!("🧠 recalled {recalled}"),
        // Say what fraction was searched, not just that something is happening:
        // "512 of 2048" tells a user whether to retry, and a spinner does not.
        HydrationStatus::Loading { loaded, total } => {
            format!("🧠 recalled {recalled} · searching {loaded} of {total} entries")
        }
        // Terminal, unlike `Loading` — retrying will not improve it, so the
        // wording must not imply progress.
        HydrationStatus::Degraded { loaded, total, .. } => {
            format!("🧠 recalled {recalled} · only {loaded} of {total} entries loaded")
        }
        // Keeps `recalled`. Dropping it hid from the user that N memories had
        // in fact been injected into the prompt, which is the opposite of the
        // transparency this line is for.
        HydrationStatus::Failed { .. } => {
            format!("🧠 recalled {recalled} · memory index unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> HydrationStatus {
        HydrationStatus::Ready { nodes: 2048 }
    }
    fn loading(loaded: usize) -> HydrationStatus {
        HydrationStatus::Loading {
            loaded,
            total: 2048,
        }
    }
    fn degraded() -> HydrationStatus {
        HydrationStatus::Degraded {
            loaded: 512,
            total: 2048,
            reason: "bad row".into(),
        }
    }
    fn failed() -> HydrationStatus {
        HydrationStatus::Failed {
            reason: "loader died".into(),
        }
    }

    /// A search that finished just as hydration did must still be qualified.
    ///
    /// This is the failing-open case: sample only after the query and the
    /// status reads `Ready`, so the caller reports plain absence about a search
    /// that ran against 100 of 2048 entries.
    #[test]
    fn test_a_query_that_raced_hydration_to_completion_is_still_qualified() {
        let seen = observed(loading(100), ready());
        assert_eq!(seen, loading(100), "the completed sample must not win");
        assert!(caveat(&seen, false).is_some());
    }

    /// Between two in-flight samples the earlier one is the honest bound.
    #[test]
    fn test_a_loading_index_reports_the_count_the_search_could_have_seen() {
        let seen = observed(loading(100), loading(900));
        assert_eq!(seen, loading(100));
        let text = caveat(&seen, false).expect("a partial index owes a caveat");
        assert!(
            text.contains("at least 100"),
            "must not claim the search saw entries loaded after it ran: {text}"
        );
        assert!(
            !text.contains("900"),
            "over-counts what was searched: {text}"
        );
    }

    /// Failure discovered after the query still qualifies the answer.
    #[test]
    fn test_a_failure_during_the_query_outranks_a_healthy_first_sample() {
        assert_eq!(observed(ready(), failed()), failed());
        assert_eq!(observed(ready(), degraded()), degraded());
    }

    /// `Failed` does not mean the tree was empty, so the caveat must not say so
    /// on top of results the search actually returned.
    #[test]
    fn test_a_failed_index_that_still_returned_hits_does_not_claim_it_searched_nothing() {
        let with_hits = caveat(&failed(), true).expect("failure owes a caveat");
        assert!(
            !with_hits.contains("nothing could be searched"),
            "contradicts the results printed above it: {with_hits}"
        );
        let without_hits = caveat(&failed(), false).expect("failure owes a caveat");
        assert!(without_hits.contains("nothing could be searched"));
    }

    /// The unread remainder was not read; it was not proven unreadable.
    #[test]
    fn test_a_degraded_index_does_not_claim_the_unread_remainder_is_broken() {
        let text = caveat(&degraded(), true).expect("a partial index owes a caveat");
        assert!(
            !text.contains("unreadable"),
            "asserts a property of data it never read: {text}"
        );
    }

    /// A complete index owes no qualification at all.
    #[test]
    fn test_a_complete_index_says_nothing_extra() {
        assert!(caveat(&ready(), true).is_none());
        assert!(caveat(&ready(), false).is_none());
    }

    #[test]
    fn test_status_line_tells_the_truth_about_a_partial_index() {
        assert_eq!(status_line(3, &ready()), "🧠 recalled 3");
        assert_eq!(
            status_line(3, &loading(512)),
            "🧠 recalled 3 · searching 512 of 2048 entries"
        );
        assert_eq!(
            status_line(3, &degraded()),
            "🧠 recalled 3 · only 512 of 2048 entries loaded"
        );
    }

    /// A failed index must not erase the count of what was already injected.
    #[test]
    fn test_status_line_keeps_the_recall_count_when_the_index_failed() {
        let line = status_line(3, &failed());
        assert!(
            line.contains('3'),
            "hides that 3 memories reached the prompt: {line}"
        );
        assert!(line.contains("unavailable"), "{line}");
    }

    /// The states must be distinguishable *after* the glyph is stripped.
    ///
    /// Asserting only that each line contains letters is an assertion that
    /// cannot fail for the reason it exists: collapse all four arms to
    /// "recalled 1" and it still passes, while a screen reader now says the
    /// same sentence for a healthy index and an unreadable one. Distinctness is
    /// the property; "has words" is a side condition.
    #[test]
    fn test_status_line_is_meaningful_without_the_glyph() {
        let spoken: Vec<String> = [ready(), loading(1), degraded(), failed()]
            .iter()
            .map(|status| {
                status_line(1, status)
                    .replace('🧠', "")
                    .replace('·', "")
                    .trim()
                    .to_string()
            })
            .collect();
        for (index, line) in spoken.iter().enumerate() {
            assert!(
                line.chars().any(char::is_alphabetic),
                "every state needs words, not just a glyph: {line:?}"
            );
            for other in &spoken[index + 1..] {
                assert_ne!(
                    line, other,
                    "two hydration states speak identically, so the difference \
                     is carried by nothing a listener can hear"
                );
            }
        }
    }
}
