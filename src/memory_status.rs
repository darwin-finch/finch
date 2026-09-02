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

/// A sentence for a reader whose read did not cover the whole store, or `None`
/// when the index was complete and no qualification is owed.
///
/// Every string here has to be true in all three contexts that print it -- the
/// search tool, the inspect tool, and `/memory` -- because a sentence written
/// for one and reused by the others is how the last two rounds of this change
/// produced false claims of their own.
pub(crate) fn caveat(status: &HydrationStatus) -> Option<String> {
    match status {
        HydrationStatus::Ready { .. } => None,
        // Zero read is not "at least 0 read": it is a nothing-was-read state,
        // and pairing it with a "no matches among the entries read" lead made
        // that lead vacuous.
        HydrationStatus::Loading { loaded: 0, total } => Some(format!(
            "The memory index is still loading and none of its {total} entries have been \
             read yet. Retrying shortly may reach some."
        )),
        HydrationStatus::Loading { loaded, total } => Some(format!(
            "The memory index is still loading: at least {loaded} of {total} entries have \
             been read. Retrying shortly may reach more."
        )),
        // Not "retrying will not find more": `degrade` fires on any batch read
        // error, transient ones included, and a reload can clear the failure
        // within the same process. Stating it as permanent would be the same
        // claim-beyond-evidence as the sentence below.
        HydrationStatus::Degraded { loaded, total, .. } => Some(format!(
            "The memory index is incomplete: {loaded} of {total} entries loaded before a \
             read error stopped it, and the remainder were not read."
        )),
        // Deliberately says nothing about *how much* loaded.
        //
        // `Failed` does not mean the index is empty. It is raised both when the
        // first batch fails, where nothing loaded, and by the hydration guard's
        // `Drop`, which fires on panic, abort or runtime shutdown however much
        // had already loaded -- and `HydrationStatus::Failed` carries no count,
        // so this function cannot tell the two apart. Earlier rounds guessed,
        // via a `found_any` flag derived from whether the *caller's query*
        // matched anything, which is not a measure of the index at all: a
        // search that matched nothing among a thousand loaded entries was told
        // none had loaded. The honest sentence is the one that holds either
        // way.
        HydrationStatus::Failed { .. } => Some(
            "The memory index is unavailable: hydration did not complete, so an unknown \
             part of the store is missing from it."
                .to_string(),
        ),
    }
}

/// Whether the index had read nothing at all, so a caller must not prefix its
/// caveat with a claim about "the entries that were read".
pub(crate) fn read_nothing(status: &HydrationStatus) -> bool {
    // `Failed` is deliberately not here. Its `loaded` count is unknown, so
    // claiming nothing was read would be a guess -- the same guess that made
    // `found_any` wrong.
    matches!(status, HydrationStatus::Loading { loaded: 0, .. })
}

/// What a turn recalled, together with the index it recalled from.
///
/// The two travel as one value because separating them is what went wrong.
/// A count taken against a partial index, re-rendered later against a freshly
/// sampled status, reads as a complete recall — and the end-of-turn refresh is
/// the *last* writer to the strip, so on the ordinary startup path it replaced
/// an accurate line with a bare one at the moment the user read it.
#[derive(Clone, Debug)]
pub(crate) struct Recall {
    pub(crate) count: usize,
    pub(crate) index: HydrationStatus,
}

impl Recall {
    /// A turn with no memory system attached: nothing recalled, nothing to
    /// qualify.
    pub(crate) fn none() -> Self {
        Self {
            count: 0,
            index: HydrationStatus::Ready { nodes: 0 },
        }
    }

    pub(crate) fn line(&self) -> String {
        status_line(self.count, &self.index)
    }

    /// The line for a later refresh, which knows the live index too.
    ///
    /// Takes the worse of the two, so it can never claim a more complete index
    /// than the recall actually saw, and still surfaces a failure that happened
    /// after the recall rather than holding the old state until the next turn.
    pub(crate) fn line_against(&self, live: HydrationStatus) -> String {
        status_line(self.count, &no_more_complete_than(&self.index, live))
    }
}

/// Escalate the *kind* of a later status onto a recall, never its counts.
///
/// Not `observed`. That function compares two samples taken around a single
/// query, where the earlier sample's `loaded` is a genuine lower bound on what
/// the query read. Across a turn boundary that invariant does not hold, and
/// because `observed` picks a winner by severity and then renders *that*
/// sample's numbers, a recall taken at `Loading { loaded: 100 }` that later saw
/// the loader `Degraded { loaded: 1536 }` rendered as
/// "recalled 3 · only 1536 of 2048 entries loaded" -- advertising an index
/// fifteen times more complete than the one the three memories came from. That
/// is the over-claim this module exists to prevent, reintroduced by reusing a
/// function outside the invariant it was written for.
///
/// So a worse live status contributes its kind, and the recall keeps its own
/// counts. `Failed` carries no counts to keep.
fn no_more_complete_than(recall: &HydrationStatus, live: HydrationStatus) -> HydrationStatus {
    if severity(&live) <= severity(recall) {
        return recall.clone();
    }
    match (recall, live) {
        (HydrationStatus::Loading { loaded, total }, HydrationStatus::Degraded { reason, .. }) => {
            HydrationStatus::Degraded {
                loaded: *loaded,
                total: *total,
                reason,
            }
        }
        // A `Ready` recall has no smaller count to preserve, and `Failed` has
        // no count at all; in both the live status stands.
        (_, live) => live,
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
        // Distinct from `Loading` because hydration has stopped, so the
        // wording must not imply progress is under way. It does not say the
        // state is permanent: `degrade` fires on any batch read error,
        // transient ones included, and a reload can clear it.
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
        assert!(caveat(&seen).is_some());
    }

    /// Between two in-flight samples the earlier one is the honest bound.
    #[test]
    fn test_a_loading_index_reports_the_count_the_search_could_have_seen() {
        let seen = observed(loading(100), loading(900));
        assert_eq!(seen, loading(100));
        let text = caveat(&seen).expect("a partial index owes a caveat");
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

    /// A later, worse status must not bring its own counts to an old recall.
    ///
    /// `observed` compares two samples around one query, where the earlier
    /// `loaded` bounds what that query read. Across a turn boundary that does
    /// not hold: it picks the winner by severity and renders *that* sample's
    /// numbers, so a recall of 3 taken at 100 entries, followed by the loader
    /// degrading at 1536, advertised an index fifteen times more complete than
    /// the one those three memories came from.
    #[test]
    fn test_a_later_failure_does_not_inflate_what_the_recall_saw() {
        let recall = Recall {
            count: 3,
            index: loading(100),
        };
        let line = recall.line_against(HydrationStatus::Degraded {
            loaded: 1536,
            total: 2048,
            reason: "late batch".into(),
        });
        assert!(
            line.contains("100 of 2048"),
            "claimed a more complete index than the recall saw: {line}"
        );
        assert!(!line.contains("1536"), "{line}");
        // The escalation itself still happens: the line must not read as though
        // loading were merely still under way.
        assert!(
            line.contains("only"),
            "did not adopt the worse kind: {line}"
        );
    }

    /// A live status no worse than the recall's changes nothing.
    #[test]
    fn test_a_recovered_index_does_not_erase_the_recalls_qualification() {
        let recall = Recall {
            count: 3,
            index: loading(100),
        };
        assert_eq!(recall.line_against(ready()), recall.line());
    }

    /// Zero entries read is not "at least 0 read".
    #[test]
    fn test_an_index_that_has_read_nothing_yet_does_not_talk_about_entries_read() {
        assert!(read_nothing(&loading(0)));
        assert!(!read_nothing(&loading(1)));
        assert!(!read_nothing(&degraded()));
        assert!(!read_nothing(&ready()));

        let text = caveat(&loading(0)).expect("a partial index owes a caveat");
        assert!(
            !text.contains("at least 0"),
            "reports a count of entries it never read: {text}"
        );
    }

    /// `Failed` must not be described as an empty index.
    ///
    /// The hydration guard's `Drop` raises it however much had loaded, and
    /// `HydrationStatus::Failed` carries no count -- so any sentence about
    /// *how much* loaded is a guess. Earlier rounds guessed from whether the
    /// caller's query matched anything, which is not a measure of the index:
    /// a search that matched nothing among a thousand loaded entries was told
    /// none had loaded.
    #[test]
    fn test_the_failure_sentence_does_not_guess_how_much_had_loaded() {
        let text = caveat(&failed()).expect("failure owes a caveat");
        assert!(
            !text.contains("none of"),
            "asserts an empty index it cannot know is empty: {text}"
        );
        assert!(
            !text.contains("nothing could be read"),
            "denies reads that succeeded on the same screen: {text}"
        );
        assert!(
            !read_nothing(&failed()),
            "an unknown loaded count is not a known zero"
        );
        assert!(text.contains("unknown"), "{text}");
    }

    /// A complete index owes no qualification at all.
    #[test]
    fn test_a_complete_index_says_nothing_extra() {
        assert!(caveat(&ready()).is_none());
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
                    .replace(['🧠', '·'], "")
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
