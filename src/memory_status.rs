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

/// The status that describes what a read could actually have seen, given a
/// sample taken before it ran and one taken after.
///
/// Sampling only afterwards fails *open*, which is the dangerous direction: if
/// hydration finishes between the read and the sample, the status reads `Ready`
/// and the caller reports "no memories found" about a search that covered a
/// fraction of the store. Hydration also advances during the read -- it yields
/// between batches, and `query_with_sources` contends for the same locks -- so
/// an after-sample's `loaded` is a strict over-count of what was read.
///
/// So the later sample contributes only its *kind*, never its counts. Taking
/// the whole later sample was a subtler form of the same over-claim: with
/// `before = Loading { loaded: 100 }` and the loader degrading at 1536 during
/// the read, it rendered "recalled 3 · only 1536 of 2048 entries loaded"
/// for a read whose true coverage was somewhere in [100, 1536]. The earlier
/// count is the only honest figure, and every string that prints it says "at
/// least" for that reason.
pub(crate) fn observed(before: HydrationStatus, after: HydrationStatus) -> HydrationStatus {
    if severity(&after) <= severity(&before) {
        return before;
    }
    match (before, after) {
        (HydrationStatus::Loading { loaded, total }, HydrationStatus::Degraded { reason, .. }) => {
            HydrationStatus::Degraded {
                loaded,
                total,
                reason,
            }
        }
        // `Failed` carries no counts, so the later status stands. So does a
        // worse status behind a `Ready` sample: that pair would render the later
        // counts as though they described the read, which *understates* a
        // complete read rather than inflating a partial one. It is unreachable
        // today -- `done` is monotone, so nothing leaves `Ready` for a worse
        // state in-process -- and understating is the safe direction if it ever
        // becomes reachable.
        (_, after) => after,
    }
}

/// A sentence for a reader whose read did not cover the whole store, or `None`
/// when the index was complete and no qualification is owed.
///
/// Every string here has to be true in all four contexts that print it -- the
/// search tool, the inspect tool, `/memory`, and `mem-recall`'s warning log --
/// because a sentence written for one and reused by the others is how several
/// rounds of this change produced false claims of their own.
pub(crate) fn caveat(status: &HydrationStatus) -> Option<String> {
    match status {
        HydrationStatus::Ready { .. } => None,
        // No count at all when the bound is zero.
        //
        // Every other wording here is a lower bound, which survives the fact
        // that the earlier sample under-counts what the read actually covered.
        // At zero that framing collapses into an absolute -- "none have been
        // read" -- and the read may since have matched three memories, printed
        // directly above this sentence. A vacuous bound is worse than no bound.
        HydrationStatus::Loading { loaded: 0, total } => Some(format!(
            "The memory index was still loading when this read began, with {total} \
             entries to load. Retrying shortly may reach more."
        )),
        HydrationStatus::Loading { loaded, total } => Some(format!(
            "The memory index was still loading when this read began: this read covered \
             at least {loaded} of {total} entries. Retrying shortly may reach more."
        )),
        // Not "retrying will not find more": `degrade` fires on any batch read
        // error, transient ones included, and a reload can clear the failure
        // within the same process. Stating it as permanent would be the same
        // claim-beyond-evidence as the sentence below.
        // Reachable through `observed`'s count-carry when a read that began at
        // zero meets a degrading loader. Vacuous rather than false, but the
        // `Loading` arm above special-cases exactly this for exactly this
        // reason: a bound of zero bounds nothing.
        HydrationStatus::Degraded {
            loaded: 0, total, ..
        } => Some(format!(
            "A read error stopped the memory index short, which had {total} entries \
             to load."
        )),
        // Coverage-centric, not index-centric.
        //
        // "{loaded} of {total} entries loaded" was a claim about the index, and
        // the count here is not the index's: when hydration degrades *during* a
        // read, `observed` keeps the earlier sample's count, which is a lower
        // bound on what the read saw and lower than what finally loaded. Only a
        // sentence about the read is true of both.
        HydrationStatus::Degraded { loaded, total, .. } => Some(format!(
            "A read error stopped the memory index short: this read covered at least \
             {loaded} of {total} entries, and how much beyond that is unknown."
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
            "The memory index did not finish loading, so how much of the store it holds \
             is unknown."
                .to_string(),
        ),
    }
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
    /// Takes the worse *kind* and the recall's own *counts* -- not simply "the
    /// worse of the two", which is what round 4 got wrong. So it can never
    /// claim a more complete index than the recall saw, and still surfaces a
    /// failure that happened after the recall rather than holding the old state
    /// until the next turn.
    pub(crate) fn line_against(&self, live: HydrationStatus) -> String {
        status_line(self.count, &observed(self.index.clone(), live))
    }
}

/// A parenthetical for a surface that prints the loaded node count with no room
/// for a sentence.
///
/// `MemoryStats::tree_node_count` is the size of the in-memory tree, so during
/// hydration it is a count of what has loaded, not of what the user stored --
/// a wrong number about their own data. `/memory` has room to say so; the
/// provider-switch line and `/model show` do not, and printing the number bare
/// is what this change exists to stop.
pub(crate) fn count_qualifier(status: &HydrationStatus) -> Option<&'static str> {
    match status {
        HydrationStatus::Ready { .. } => None,
        HydrationStatus::Loading { .. } => Some("so far; the index was still loading"),
        HydrationStatus::Degraded { .. } => Some("so far; a read error stopped the index short"),
        HydrationStatus::Failed { .. } => Some("so far; the index did not finish loading"),
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
        // "searching 0 of 2048" beside a nonzero recall is refuted by its own
        // line, so the zero case names no count.
        HydrationStatus::Loading { loaded: 0, total } => {
            format!("🧠 recalled {recalled} · index was still loading, {total} entries total")
        }
        HydrationStatus::Loading { loaded, total } => {
            format!("🧠 recalled {recalled} · searched at least {loaded} of {total} entries")
        }
        // Distinct from `Loading` because hydration has stopped, so the
        // wording must not imply progress is under way. It does not say the
        // state is permanent: `degrade` fires on any batch read error,
        // transient ones included, and a reload can clear it.
        HydrationStatus::Degraded {
            loaded: 0, total, ..
        } => {
            format!("🧠 recalled {recalled} · {total} entries total · index stopped short")
        }
        HydrationStatus::Degraded { loaded, total, .. } => {
            format!(
                "🧠 recalled {recalled} · searched at least {loaded} of {total} entries · index stopped short"
            )
        }
        // Keeps `recalled`. Dropping it hid from the user that N memories had
        // in fact been injected into the prompt, which is the opposite of the
        // transparency this line is for.
        HydrationStatus::Failed { .. } => {
            format!("🧠 recalled {recalled} · index did not finish loading")
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
            line.contains("stopped short"),
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

    /// The recall site must not inflate either, not just the later refresh.
    ///
    /// Round 4 fixed the refresh; the same over-claim survived at the recall,
    /// because `observed` returned the whole later sample when the kinds
    /// differed, so an inflated status was baked into `Recall.index` before any
    /// refresh could correct it. With `before = Loading { loaded: 100 }` and the
    /// loader degrading at 1536 mid-read, the strip advertised an index fifteen
    /// times more complete than the one the recall saw.
    #[test]
    fn test_a_degradation_during_the_read_keeps_the_earlier_count() {
        let seen = observed(
            loading(100),
            HydrationStatus::Degraded {
                loaded: 1536,
                total: 2048,
                reason: "late batch".into(),
            },
        );
        assert_eq!(
            seen,
            HydrationStatus::Degraded {
                loaded: 100,
                total: 2048,
                reason: "late batch".into()
            },
            "took the later sample's count for a read that could not have seen it"
        );
        let text = caveat(&seen).expect("a partial index owes a caveat");
        assert!(text.contains("at least 100"), "{text}");
        assert!(!text.contains("1536"), "{text}");
        // And nothing about the remainder. "the rest were not read" closes an
        // upper bound the same sentence has just left open, and is false: 1436
        // of "the rest" did load. A bound cannot end in an absolute.
        assert!(
            text.contains("how much beyond that is unknown"),
            "the tail must stay a bound; asserting only the absence of the old \
             literal would pass for any other absolute: {text}"
        );
        // Index-centric wording would be false of this count: 1536 entries
        // loaded, not 100. Only a sentence about the read is true of both.
        assert!(
            text.contains("this read covered"),
            "describes the index rather than the read: {text}"
        );
    }

    /// A zero lower bound must not be stated as an absolute.
    ///
    /// `observed` returns the earlier sample, so "none have been read" is
    /// emitted from a sample taken before the read ran -- and the read may have
    /// matched three memories printed directly above it. Every other wording
    /// here is a bound, which survives being stale; at zero that framing
    /// collapses into a claim its own output refutes.
    #[test]
    fn test_a_zero_lower_bound_is_not_stated_as_an_absolute() {
        let text = caveat(&loading(0)).expect("a partial index owes a caveat");
        assert!(
            !text.contains("none of"),
            "asserts nothing was read, beside results that were: {text}"
        );
        // Nor a claim that the coverage is unknown. `/memory` prints the
        // covered count one line above this sentence, so "how many … is
        // unknown" is contradicted by the screen it appears on -- the same
        // shape as the zero bound it replaced, in the other direction.
        assert!(
            !text.contains("unknown"),
            "calls unknown a number /memory displays directly above it: {text}"
        );
        // And no prediction about a retry. This arm knows least -- a loader
        // that fails at zero reaches nothing more, ever -- so it must hedge at
        // least as hard as the arm that knows the loader is progressing.
        assert!(
            text.contains("may reach more"),
            "promises a retry will help, from the state with the least evidence \
             that it will: {text}"
        );

        let line = status_line(3, &loading(0));
        assert!(
            !line.contains("0 of 2048"),
            "a line refuted by its own recall count: {line}"
        );
        assert!(line.contains('3'), "{line}");
    }

    /// Zero entries read is not reported as "at least 0 read".
    #[test]
    fn test_an_index_that_has_read_nothing_yet_does_not_talk_about_entries_read() {
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
        assert!(text.contains("unknown"), "{text}");
    }

    /// Nothing here may claim, in the present tense, that loading is under way.
    ///
    /// `observed` returns the earlier sample whenever the later one is no
    /// worse, so a partial status is routinely rendered *after* hydration has
    /// finished -- and `line_against` holds one deliberately for a whole turn,
    /// which `test_a_recovered_index_does_not_erase_the_recalls_qualification`
    /// asserts. A progress claim in the present tense is falsified by the very
    /// sample the renderer is holding.
    ///
    /// The first version of this test asserted the absence of one literal,
    /// `"is still loading"`. That is a regression test for the string round 8
    /// removed, not for the property in its name: of the three arms round 9
    /// changed, only one contained that literal, so the sweep would have caught
    /// one of them and the commit message claimed it covered all three. This
    /// version checks the property across every renderer and every state, so
    /// `"the index is loading"` or `"loading is in progress"` fails too.
    #[test]
    fn test_no_rendering_claims_loading_is_still_under_way() {
        const PRESENT_TENSE: &[&str] = &[
            "is still loading",
            "is loading",
            "is being loaded",
            "is in progress",
            "loading is",
            "is still being",
            "currently loading",
        ];
        for status in [ready(), loading(0), loading(1), degraded(), failed()] {
            let mut rendered = vec![status_line(1, &status)];
            rendered.extend(caveat(&status));
            rendered.extend(count_qualifier(&status).map(str::to_string));
            for text in rendered {
                let lowered = text.to_lowercase();
                for marker in PRESENT_TENSE {
                    assert!(
                        !lowered.contains(marker),
                        "{marker:?} is a present-tense progress claim, falsified by the \
                         sample being rendered: {text}"
                    );
                }
            }
        }
    }

    /// `count_qualifier` is what the surfaces with no room for a sentence show.
    #[test]
    fn test_count_qualifier_speaks_for_every_partial_state() {
        assert!(count_qualifier(&ready()).is_none());
        for status in [loading(0), loading(1), degraded(), failed()] {
            let note = count_qualifier(&status)
                .unwrap_or_else(|| panic!("a partial index owes a note: {status:?}"));
            assert!(
                note.chars().any(char::is_alphabetic) && note.len() > 6,
                "{note:?}"
            );
        }
        // The three must be distinguishable, for the same reason the status
        // line's five must be: a reader gets only this parenthetical.
        let notes: Vec<_> = [loading(1), degraded(), failed()]
            .iter()
            .map(|status| count_qualifier(status).unwrap())
            .collect();
        assert_ne!(notes[0], notes[1]);
        assert_ne!(notes[1], notes[2]);
        assert_ne!(notes[0], notes[2]);
    }

    /// A degraded index whose bound is zero bounds nothing, same as `Loading`.
    #[test]
    fn test_a_degraded_index_with_a_zero_bound_states_no_count() {
        let zero = HydrationStatus::Degraded {
            loaded: 0,
            total: 2048,
            reason: "first batch".into(),
        };
        let text = caveat(&zero).expect("a partial index owes a caveat");
        assert!(
            !text.contains("at least 0"),
            "a bound of zero bounds nothing: {text}"
        );
        assert!(
            !text.contains("unknown"),
            "calls unknown a number /memory displays directly above it: {text}"
        );

        let line = status_line(3, &zero);
        assert!(!line.contains("0 of 2048"), "{line}");
        assert!(line.contains("stopped short"), "{line}");
        assert!(line.contains('3'), "{line}");
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
            "🧠 recalled 3 · searched at least 512 of 2048 entries"
        );
        assert_eq!(
            status_line(3, &degraded()),
            "🧠 recalled 3 · searched at least 512 of 2048 entries · index stopped short"
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
        assert!(line.contains("did not finish loading"), "{line}");
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
        let spoken: Vec<String> = [ready(), loading(0), loading(1), degraded(), failed()]
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
