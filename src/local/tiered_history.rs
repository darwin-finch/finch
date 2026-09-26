//! Discrete-tier compaction for local-model conversation history (#1266,
//! design doc #1260).
//!
//! `TemplateGenerator::prompt_parts` (`generator.rs`) used to bound history
//! with `fit_history_to_budget` (#1241): a single hard recency cutoff that
//! drops the oldest candidate exchanges wholesale once the model's real
//! token budget (`GeneratorModel::context_length`) is exceeded. That avoids
//! the context-window crash #1234 reported, but loses old context abruptly
//! instead of degrading it, and recomputes the same drop-or-keep decision
//! from scratch on every call.
//!
//! This module replaces that cutoff with three discrete, ordered
//! compaction tiers -- `Verbatim` -> `LightlyCompressed` -> `Gist` -- and a
//! small [`TierAssigner`] that remembers each history entry's tier across
//! calls. The reason this exists at all instead of just "summarize harder"
//! is llama.cpp's own KV-cache reuse: it depends on the new prompt sharing
//! an identical prefix with the previous turn's prompt. Recomputing
//! compression continuously would rewrite early prompt bytes every turn and
//! destroy that reuse, trading "no summarization pause" for "every turn is
//! a cold full-context pass." Discrete tiers avoid this: an entry is
//! compressed once, cheaply, with no model call, when it first crosses a
//! tier boundary, then stays byte-identical at that tier until it crosses
//! the *next* boundary. `TierAssigner::assign_and_compress`'s tests assert
//! this directly (byte-stability across repeated calls at a fixed budget,
//! and monotonic-only tier advancement as the budget tightens).
//!
//! ## Why this replaces `fit_history_to_budget` outright rather than
//! composing with it
//!
//! `fit_history_to_budget` is a special case of tiering with exactly one
//! tier (`Verbatim`) and a hard drop past it. Keeping both would mean two
//! competing places decide "does this entry survive," so this module
//! subsumes it entirely: `TierAssigner::assign_and_compress` takes the same
//! `(current_question, history, budget_tokens, count_tokens)` shape
//! `fit_history_to_budget` did, drops entries the same way (oldest first,
//! current question always kept) when nothing fits even at `Gist`, and adds
//! two cheaper intermediate representations in between so more of the
//! conversation survives in *some* form under the same budget.
//!
//! ## Tier boundaries
//!
//! Boundaries are fractions of the token budget actually available for
//! history (`budget_tokens` minus the current question's own cost), not a
//! fixed absolute count -- so they scale with the model's real
//! `context_length()` the same way `fit_history_to_budget`'s single cutoff
//! already did. Walking history newest-first (matching the existing
//! recency bias), an entry is kept `Verbatim` while cumulative cost stays
//! under 50% of the history budget, `LightlyCompressed` up to 80%, `Gist`
//! up to 100%, and dropped beyond that.
//!
//! ## Compression techniques
//!
//! - `LightlyCompressed`: extractive, no model call -- the entry's first
//!   and last sentence (naive `.`/`!`/`?` splitting), joined by `[...]`.
//!   Entries with fewer than three sentences have nothing worth dropping
//!   and are kept as-is.
//! - `Gist`: the entry's top-frequency non-stopword words (a small fixed
//!   count), rendered as `[gist: term, term, ...]`. This repo already has a
//!   hashed-ngram term-weighting engine
//!   (`crates/finch-memory/src/embeddings.rs`, mid-rename from
//!   `TfIdfEmbedding` to `HashedNgramEmbedding` under #1256) that would be a
//!   strictly better source of "top terms," but that rename lives on an
//!   unmerged branch (`rename/tfidf-to-hashed-ngram-embedding`) as of this
//!   writing and `finch-memory` is not a dependency of `src/local` today.
//!   Rather than block this issue on that branch landing and a new
//!   dependency edge, `Gist` uses a standalone word-frequency count here.
//!   Swapping in `HashedNgramEmbedding` once available is a self-contained
//!   follow-up: it only needs to change what feeds `gist`'s term list, not
//!   `TierAssigner`'s tiering or state logic.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// How compressed a history entry currently is. Ordered from least to most
/// compressed (`Verbatim < LightlyCompressed < Gist` -- derived `Ord`
/// follows declaration order): an entry's tier only ever advances along
/// this order, never regresses, which is what keeps its compacted bytes
/// stable across turns and preserves llama.cpp's KV-cache reuse for the
/// untouched bulk of the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum CompactionTier {
    /// Full original text, unmodified.
    Verbatim,
    /// A cheap, deterministic extractive reduction: the entry's first and
    /// last sentence, with a `[...]` marker where the middle was dropped.
    LightlyCompressed,
    /// A short, fixed-size extractive summary: the entry's top-frequency
    /// non-stopword words.
    Gist,
}

/// Percent of the history token budget (i.e. `budget_tokens` minus the
/// current question's own cost) reserved for entries kept at `Verbatim`.
const VERBATIM_BUDGET_PERCENT: usize = 50;
/// Additional percent, on top of [`VERBATIM_BUDGET_PERCENT`], reserved for
/// `LightlyCompressed` entries.
const LIGHTLY_COMPRESSED_BUDGET_PERCENT: usize = 30;
/// Additional percent, on top of the two above, reserved for `Gist`
/// entries. The three together cover 100% of the history budget: an entry
/// that doesn't fit even as `Gist` is dropped, the same outcome
/// `fit_history_to_budget` gave past its single cutoff.
const GIST_BUDGET_PERCENT: usize = 20;

/// Minimum sentence count an entry must have before `LightlyCompressed`
/// drops anything; shorter entries are already cheap and are kept as-is.
const LIGHT_COMPRESSION_MIN_SENTENCES: usize = 3;

/// How many top-frequency terms `Gist` keeps.
const GIST_TOP_TERMS: usize = 6;

/// Minimum word length `Gist` considers (drops short function words like
/// "a", "is", "to" without needing a stopword entry for each one).
const GIST_MIN_WORD_LEN: usize = 3;

/// Small stopword list for `Gist`'s term extraction. Not exhaustive by
/// design -- this is a cheap extractive heuristic, not a real NLP
/// pipeline; see the module doc for the standalone-vs-`HashedNgramEmbedding`
/// tradeoff.
const GIST_STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "that",
    "this",
    "with",
    "you",
    "your",
    "have",
    "has",
    "had",
    "are",
    "was",
    "were",
    "but",
    "not",
    "can",
    "will",
    "would",
    "could",
    "should",
    "from",
    "they",
    "them",
    "their",
    "what",
    "when",
    "where",
    "which",
    "who",
    "whom",
    "how",
    "why",
    "there",
    "here",
    "then",
    "than",
    "into",
    "onto",
    "about",
    "some",
    "any",
    "all",
    "each",
    "just",
    "like",
    "user",
    "assistant",
];

/// Splits a formatted history entry (`"role: text"`, the shape
/// `TemplateGenerator::prompt_parts` builds) into its role prefix
/// (including the `": "` separator) and body. Entries with no `": "`
/// separator are treated as having no prefix.
fn split_role_prefix(entry: &str) -> (&str, &str) {
    match entry.find(": ") {
        Some(idx) => (&entry[..idx + 2], &entry[idx + 2..]),
        None => ("", entry),
    }
}

/// Naive sentence splitter on `.`/`!`/`?`. Cheap and extractive by design;
/// it does not handle abbreviations or other edge cases, which is an
/// acceptable tradeoff for a compression tier whose whole purpose is
/// avoiding a model call.
fn split_sentences(body: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let bytes = body.as_bytes();
    for (i, &byte) in bytes.iter().enumerate() {
        if byte == b'.' || byte == b'!' || byte == b'?' {
            let end = i + 1;
            let candidate = body[start..end].trim();
            if !candidate.is_empty() {
                sentences.push(candidate);
            }
            start = end;
        }
    }
    let tail = body[start..].trim();
    if !tail.is_empty() {
        sentences.push(tail);
    }
    sentences
}

/// `LightlyCompressed` technique: first and last sentence, `[...]` in
/// between. Entries too short to have a meaningful middle are returned
/// unchanged.
fn lightly_compress(entry: &str) -> String {
    let (prefix, body) = split_role_prefix(entry);
    let sentences = split_sentences(body);
    if sentences.len() < LIGHT_COMPRESSION_MIN_SENTENCES {
        return entry.to_string();
    }
    let first = sentences.first().expect("checked len >= minimum above");
    let last = sentences.last().expect("checked len >= minimum above");
    format!("{prefix}{first} [...] {last}")
}

/// `Gist` technique: top-frequency non-stopword words, ties broken by
/// first occurrence so the result is deterministic regardless of
/// `HashMap` iteration order.
fn gist(entry: &str) -> String {
    let (prefix, body) = split_role_prefix(entry);
    let mut counts: HashMap<String, (usize, usize)> = HashMap::new();
    let mut next_order = 0usize;
    for raw_word in body.split_whitespace() {
        let word: String = raw_word
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        if word.len() < GIST_MIN_WORD_LEN || GIST_STOPWORDS.contains(&word.as_str()) {
            continue;
        }
        let slot = counts.entry(word).or_insert((0, next_order));
        slot.0 += 1;
        next_order += 1;
    }
    let mut terms: Vec<(String, usize, usize)> = counts
        .into_iter()
        .map(|(term, (count, first_seen))| (term, count, first_seen))
        .collect();
    terms.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    terms.truncate(GIST_TOP_TERMS);

    if terms.is_empty() {
        return format!("{prefix}[earlier context, condensed]");
    }
    let words: Vec<String> = terms.into_iter().map(|(term, _, _)| term).collect();
    format!("{prefix}[gist: {}]", words.join(", "))
}

/// Renders `entry` at `tier`. `Verbatim` is a no-op; the other two dispatch
/// to their extractive technique above.
pub(super) fn compress_to_tier(entry: &str, tier: CompactionTier) -> String {
    match tier {
        CompactionTier::Verbatim => entry.to_string(),
        CompactionTier::LightlyCompressed => lightly_compress(entry),
        CompactionTier::Gist => gist(entry),
    }
}

/// Joins tier-compressed history entries (oldest first) with the current
/// question the same way `fit_history_to_budget` did: history first, a
/// blank line, then the question; the question alone if no history
/// survived.
pub(super) fn join_history_and_question(current_question: &str, included: Vec<String>) -> String {
    if included.is_empty() {
        current_question.to_string()
    } else {
        format!("{}\n\n{}", included.join("\n\n"), current_question)
    }
}

/// Stable content key for a history entry. Entries are immutable once
/// `prompt_parts` produces them, so the same text always identifies the
/// same underlying exchange across calls. `DefaultHasher`'s algorithm is
/// not a durability guarantee across process restarts -- by design (#1266):
/// this state is in-memory only for the life of one `TemplateGenerator`,
/// and cross-restart persistence is separate, later work (the Brain-journal
/// marker, #1269).
fn content_key(entry: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    entry.hash(&mut hasher);
    hasher.finish()
}

/// Tries `entry` at `floor` and every more-compressed tier after it, in
/// order, returning the first that fits `used` plus its own cost within
/// that tier's cumulative budget limit. Returns `None` if it doesn't fit
/// even as `Gist`. Never considers a tier less compressed than `floor` --
/// that is what makes tier advancement monotonic in
/// [`TierAssigner::assign_and_compress`].
fn fit_from_floor(
    entry: &str,
    used: usize,
    floor: CompactionTier,
    verbatim_limit: usize,
    light_limit: usize,
    gist_limit: usize,
    count_tokens: &impl Fn(&str) -> usize,
) -> Option<CompactionTier> {
    if floor <= CompactionTier::Verbatim {
        let cost = count_tokens(entry);
        if used.saturating_add(cost) <= verbatim_limit {
            return Some(CompactionTier::Verbatim);
        }
    }
    if floor <= CompactionTier::LightlyCompressed {
        let cost = count_tokens(&lightly_compress(entry));
        if used.saturating_add(cost) <= light_limit {
            return Some(CompactionTier::LightlyCompressed);
        }
    }
    let cost = count_tokens(&gist(entry));
    if used.saturating_add(cost) <= gist_limit {
        return Some(CompactionTier::Gist);
    }
    None
}

/// Per-session state that remembers each history entry's previously
/// assigned tier, keyed by content, so:
///
/// 1. Repeated calls with the same entry at the same effective budget
///    produce byte-identical compressed output (the KV-cache-reuse
///    property #1260 cares about).
/// 2. An entry's tier only ever advances (more compressed), never
///    regresses, even if the budget grows on a later call.
///
/// In-memory only, by design (#1266) -- cross-restart durability is
/// separate, later work.
#[derive(Debug, Default)]
pub(super) struct TierAssigner {
    assigned: HashMap<u64, CompactionTier>,
}

impl TierAssigner {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Assigns each entry in `history` (oldest first) a compaction tier
    /// against `budget_tokens`, and returns the tier-compressed strings in
    /// the same order.
    ///
    /// Walks `history` newest-first (matching `fit_history_to_budget`'s
    /// existing recency bias): each entry is tried starting from its
    /// previously assigned tier (or `Verbatim`, if never seen), advancing
    /// through `LightlyCompressed` then `Gist` until one fits the running
    /// total, and dropped if even `Gist` doesn't fit. Dropped entries are
    /// not persisted as a tier -- if a later call's budget or history shape
    /// lets one fit again, it is reconsidered fresh; only *assigned* tiers
    /// are frozen.
    pub(super) fn assign_and_compress(
        &mut self,
        current_question: &str,
        history: Vec<String>,
        budget_tokens: usize,
        count_tokens: impl Fn(&str) -> usize,
    ) -> Vec<String> {
        let question_cost = count_tokens(current_question);
        let history_budget = budget_tokens.saturating_sub(question_cost);
        let verbatim_limit = history_budget * VERBATIM_BUDGET_PERCENT / 100;
        let light_limit = verbatim_limit + history_budget * LIGHTLY_COMPRESSED_BUDGET_PERCENT / 100;
        let gist_limit = light_limit + history_budget * GIST_BUDGET_PERCENT / 100;

        let mut used = 0usize;
        let mut kept: Vec<(usize, String)> = Vec::new();
        for (age_index, entry) in history.iter().enumerate().rev() {
            let key = content_key(entry);
            let floor = self
                .assigned
                .get(&key)
                .copied()
                .unwrap_or(CompactionTier::Verbatim);
            let Some(tier) = fit_from_floor(
                entry,
                used,
                floor,
                verbatim_limit,
                light_limit,
                gist_limit,
                &count_tokens,
            ) else {
                continue;
            };
            let text = compress_to_tier(entry, tier);
            used += count_tokens(&text);
            self.assigned.insert(key, tier);
            kept.push((age_index, text));
        }
        kept.sort_by_key(|(age_index, _)| *age_index);
        kept.into_iter().map(|(_, text)| text).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> usize {
        text.split_whitespace().count()
    }

    // ── Compression techniques ────────────────────────────────────────

    #[test]
    fn lightly_compressed_keeps_first_and_last_sentence_with_marker() {
        let entry =
            "user: First sentence here. Second sentence in the middle. Third and final sentence.";
        let compressed = lightly_compress(entry);
        assert_eq!(
            compressed, "user: First sentence here. [...] Third and final sentence.",
            "unexpected lightly-compressed output: {compressed:?}"
        );
    }

    #[test]
    fn lightly_compressed_leaves_short_entries_unchanged() {
        let entry = "user: just one short sentence";
        assert_eq!(
            lightly_compress(entry),
            entry,
            "an entry with nothing meaningful to drop must be returned as-is"
        );
    }

    #[test]
    fn gist_keeps_top_frequency_non_stopword_terms_deterministically() {
        let entry = "user: rust rust rust ownership borrow checker ownership the and for";
        let compressed = gist(entry);
        assert_eq!(
            compressed, "user: [gist: rust, ownership, borrow, checker]",
            "gist must rank by frequency, break ties by first occurrence, and drop \
             stopwords/short words: {compressed:?}"
        );
    }

    #[test]
    fn gist_falls_back_to_condensed_marker_when_nothing_survives_filtering() {
        let entry = "user: a to is";
        assert_eq!(
            gist(entry),
            "user: [earlier context, condensed]",
            "an entry with no terms surviving the stopword/length filter must use the \
             explicit condensed marker"
        );
    }

    // ── Byte-stability (the core KV-cache-reuse property) ─────────────

    #[test]
    fn same_entry_at_same_budget_produces_byte_identical_output_across_calls() {
        let history = vec![
            "user: Ask about rust ownership and borrowing semantics in detail please.".to_string(),
            "assistant: Ownership tracks a single owner. Borrowing lends access temporarily. \
             Both are enforced at compile time by the borrow checker."
                .to_string(),
        ];
        let mut assigner = TierAssigner::new();

        let first = assigner.assign_and_compress("current question", history.clone(), 20, words);
        let second = assigner.assign_and_compress("current question", history, 20, words);

        assert_eq!(
            first, second,
            "identical history at an identical budget must produce byte-identical \
             compressed output on repeated calls, or llama.cpp's KV-cache reuse breaks: \
             first={first:?} second={second:?}"
        );
    }

    #[test]
    fn tier_never_regresses_when_budget_grows_after_compaction() {
        // 20 words verbatim -- see `entry_moves_to_lightly_compressed_once_past_the_verbatim_fraction`
        // below for the same entry's exact word accounting.
        let long_entry = "assistant: First sentence of a long reply. Second sentence adds \
             detail. Third sentence wraps up the point being made here."
            .to_string();
        let mut assigner = TierAssigner::new();

        // budget 30: history_budget = 30 - 2 = 28; verbatim_limit = 14. The
        // 20-word entry doesn't fit verbatim (20 > 14) but its
        // lightly-compressed form (17 words) fits under light_limit (22),
        // so this call compacts it away from Verbatim.
        let tight =
            assigner.assign_and_compress("current question", vec![long_entry.clone()], 30, words);
        assert!(
            !tight.is_empty(),
            "the entry must survive in some compressed form, not be dropped outright, at \
             this budget: long_entry={long_entry:?}"
        );
        assert_ne!(
            tight[0], long_entry,
            "a tight budget must compact the entry away from Verbatim: {tight:?}"
        );
        let compacted_tier_bytes = tight[0].clone();

        // Budget now comfortably covers the entry at Verbatim -- but the
        // prior compaction must NOT be undone.
        let roomy = assigner.assign_and_compress(
            "current question",
            vec![long_entry.clone()],
            1_000,
            words,
        );
        assert_eq!(
            roomy[0], compacted_tier_bytes,
            "an entry must never un-compact even when the budget later grows enough to \
             fit it verbatim again: tight={compacted_tier_bytes:?} roomy={roomy:?}"
        );
        assert_ne!(
            roomy[0], long_entry,
            "the roomy-budget result must still be the previously compacted bytes, not \
             the original verbatim text: {roomy:?}"
        );
    }

    // ── Tier assignment as history ages relative to budget ─────────────

    #[test]
    fn entry_stays_verbatim_within_the_verbatim_budget_fraction() {
        // history_budget = 20 - 2 = 18; verbatim_limit = 9.
        let entry = "user: short recent turn".to_string(); // 4 tokens (word-count)
        let mut assigner = TierAssigner::new();
        let out = assigner.assign_and_compress("current question", vec![entry.clone()], 20, words);
        assert_eq!(
            out,
            vec![entry],
            "an entry well within the verbatim budget fraction must stay unmodified: {out:?}"
        );
    }

    #[test]
    fn entry_moves_to_lightly_compressed_once_past_the_verbatim_fraction() {
        // budget 30: history_budget = 30 - 2 = 28; verbatim_limit = 14;
        // light_limit = 14 + 8 = 22. The entry below is 20 words verbatim
        // ("assistant:" + 19 body words across 3 sentences), which doesn't
        // fit under verbatim_limit (14), but its lightly-compressed form --
        // "assistant: First sentence of a long reply. [...] Third sentence
        // wraps up the point being made here." -- is 17 words, which fits
        // under light_limit (22).
        let entry = "assistant: First sentence of a long reply. Second sentence adds \
             detail. Third sentence wraps up the point being made here."
            .to_string();
        let mut assigner = TierAssigner::new();
        let out = assigner.assign_and_compress("current question", vec![entry.clone()], 30, words);
        assert_ne!(
            out[0], entry,
            "an entry past the verbatim fraction must be compressed, not kept verbatim: \
             {out:?}"
        );
        assert!(
            out[0].contains("[...]"),
            "past the verbatim fraction but within the lightly-compressed fraction, the \
             entry must use the LightlyCompressed technique: {out:?}"
        );
    }

    #[test]
    fn entry_moves_to_gist_once_past_the_lightly_compressed_fraction() {
        let mut assigner = TierAssigner::new();
        // Two long entries: the newest consumes enough of the light-tier
        // budget that the older one can't fit even as LightlyCompressed,
        // only as Gist.
        let newer = "assistant: Newer reply first sentence padding words here now today. \
             Newer reply second sentence more padding words appear here too. Newer reply \
             third sentence closes this out completely now."
            .to_string();
        let older = "user: Older question first sentence with several padding words here. \
             Older question second sentence adds more padding text now. Older question \
             third sentence finally closes out this turn."
            .to_string();
        let out = assigner.assign_and_compress(
            "current question",
            vec![older.clone(), newer.clone()],
            34,
            words,
        );
        assert!(
            out[1].contains("[...]") || out[1] == newer,
            "the newest entry should be Verbatim or LightlyCompressed, not further \
             degraded first: {out:?}"
        );
        assert!(
            out[0].starts_with("user: [gist:"),
            "the older entry, once it no longer fits as LightlyCompressed, must degrade \
             to Gist rather than being dropped outright while budget for Gist remains: \
             {out:?}"
        );
    }

    #[test]
    fn entry_is_dropped_once_it_does_not_fit_even_as_gist() {
        let entry = "user: ancient".to_string();
        let mut assigner = TierAssigner::new();
        // budget_tokens equal to the question's own cost leaves a
        // zero-token history budget: nothing can fit at any tier.
        let out = assigner.assign_and_compress(
            "current question",
            vec![entry],
            words("current question"),
            words,
        );
        assert!(
            out.is_empty(),
            "an entry that fits at no tier (zero history budget) must be dropped \
             entirely, never partially rendered: {out:?}"
        );
    }

    #[test]
    fn current_question_is_never_part_of_the_droppable_history() {
        // Regression shape for fit_history_to_budget's prior guarantee:
        // the question itself is never in `history` and is joined back on
        // by the caller (`join_history_and_question`), so it survives even
        // when it alone would exceed budget.
        let mut assigner = TierAssigner::new();
        let out = assigner.assign_and_compress(
            "a question that alone exceeds the tiny budget",
            vec!["user: some history".to_string()],
            1,
            words,
        );
        let joined =
            join_history_and_question("a question that alone exceeds the tiny budget", out);
        assert_eq!(
            joined, "a question that alone exceeds the tiny budget",
            "the current question must survive even when it alone exceeds budget: {joined:?}"
        );
    }

    // ── Graceful degradation vs. a hard cutoff ──────────────────────────

    #[test]
    fn long_conversation_retains_more_entries_via_tiering_than_a_hard_cutoff_would() {
        let mut history = Vec::new();
        for i in 0..8 {
            history.push(format!(
                "user: turn {i} first sentence with some padding words in it here. \
                 turn {i} second sentence adds more padding text now. turn {i} third \
                 sentence closes out this turn completely."
            ));
        }
        let budget = 40;
        let question = "current question";

        // What a naive hard cutoff (fit_history_to_budget's old algorithm:
        // keep verbatim entries newest-first until the budget is exceeded,
        // drop the rest) would have kept.
        let mut hard_cutoff_used = words(question);
        let mut hard_cutoff_kept = 0usize;
        for entry in history.iter().rev() {
            let cost = words(entry);
            if hard_cutoff_used + cost > budget {
                break;
            }
            hard_cutoff_used += cost;
            hard_cutoff_kept += 1;
        }

        let mut assigner = TierAssigner::new();
        let tiered = assigner.assign_and_compress(question, history, budget, words);

        assert!(
            tiered.len() > hard_cutoff_kept,
            "tiering must retain more entries (in some compressed form) than a hard \
             recency cutoff at the same budget: hard_cutoff_kept={hard_cutoff_kept} \
             tiered_kept={} tiered={tiered:?}",
            tiered.len()
        );
        // The core invariant tiering must never break: total cost still
        // fits the budget.
        let total: usize = words(question) + tiered.iter().map(|entry| words(entry)).sum::<usize>();
        assert!(
            total <= budget,
            "tiered output plus the current question must still fit the budget: \
             total={total} budget={budget} tiered={tiered:?}"
        );
    }
}
