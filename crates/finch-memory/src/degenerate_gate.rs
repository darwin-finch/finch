// Pre-indexing quality gate (#1323): reject a candidate memory document at
// insertion time when it is genuinely degenerate -- not merely templated or
// repetitive, which is a different case this gate must NOT catch (see the
// module doc on `DegenerateContentGate` below).
//
// This is a new, complementary capability alongside `quality::MemoryClassifier`'s
// existing text-pattern `Discard` heuristic (acks, greetings, one-word
// replies), not a replacement for it: `project_stored_conversation_inner`
// (`lib.rs`) runs the classifier first and only offers this gate the content
// the classifier already decided is worth considering.

use crate::embeddings::HashedNgramEmbedding;
use crate::stats::WelfordStats;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use std::collections::HashMap;
use std::io::Write;
use tracing::debug;

/// Minimum number of PRIOR documents that must already be reflected in a
/// metric's running baseline before that metric may contribute to a
/// rejection. Below this, a running mean/stddev is built from too little
/// history to be a meaningful baseline rather than noise -- the gate stays
/// permissive (never rejects) until both metrics clear it.
const MIN_SAMPLES: u64 = 15;

/// How many standard deviations below a metric's running mean a candidate's
/// value must fall for that metric to count as "low" in the AND-gate below.
const REJECTION_STDDEVS: f64 = 2.0;

/// Rejects a candidate memory document at insertion time when it is
/// genuinely degenerate, using two independent per-document signals:
///
/// 1. **Compression ratio** -- DEFLATE-compressed length over raw length, at
///    max compression. No tokenizer needed; catches redundant/patterned byte
///    structure regardless of vocabulary.
/// 2. **Order-0 Shannon entropy** over token frequency, using the same
///    tokenizer [`HashedNgramEmbedding`] uses for TF-IDF (`embeddings.rs`),
///    so both this gate and the embedding agree on what a "term" is.
///
/// Each signal has its own running mean/stddev, tracked online via
/// [`WelfordStats`] (constant memory, no stored history, one instance per
/// signal). A document is rejected only when BOTH signals fall more than
/// [`REJECTION_STDDEVS`] standard deviations below their OWN running mean,
/// and only once each baseline has seen at least [`MIN_SAMPLES`] prior
/// documents.
///
/// # Why both signals, not one
///
/// The two metrics catch different failure modes, and only one combination
/// is genuine spam:
///
/// - **Low compression AND low entropy** -- a single repeated token, or
///   bulk line noise: genuinely degenerate. Reject.
/// - **Low compression, high/normal entropy** -- repeated STRUCTURE with
///   varied vocabulary (a template: e.g. a fixed field label repeated over
///   many distinct values). This is not spam; it may warrant deduplication
///   or a different concern entirely, but this gate must not reject it the
///   same way.
///
/// Compression ratio alone cannot tell these apart -- both compress well.
/// Entropy is what separates "the same word/byte over and over" from "the
/// same shape holding different words each time". Requiring BOTH signals to
/// be low is therefore load-bearing, not an incidental strengthening: it is
/// exactly what keeps a templated-but-varied document out of this gate's
/// rejection.
///
/// # Lifetime and reuse
///
/// One instance is meant to live for the lifetime of one `MemorySystem` and
/// observe every candidate document exactly once, via [`Self::observe`], in
/// insertion order -- the same single-document, single-call-site discipline
/// [`crate::EmbeddingEngine::observe_document`] documents for corpus-wide TF-IDF
/// statistics. Observing the same document twice, or sharing one instance
/// across unrelated corpora, would corrupt the running baseline the same way
/// double-counting would corrupt `HashedNgramEmbedding`'s document frequency.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DegenerateContentGate {
    compression: WelfordStats,
    entropy: WelfordStats,
}

impl DegenerateContentGate {
    /// A gate with no observations yet -- permissive until [`MIN_SAMPLES`]
    /// documents have passed through [`Self::observe`].
    pub(crate) fn new() -> Self {
        Self {
            compression: WelfordStats::new(),
            entropy: WelfordStats::new(),
        }
    }

    /// Evaluate `text` against the running baselines, fold its metrics into
    /// both of them (so every observed document counts toward the baseline
    /// the NEXT candidate is judged against, regardless of this one's own
    /// verdict), and report whether it is genuinely degenerate.
    ///
    /// The verdict compares `text`'s own metrics against each baseline as it
    /// stood BEFORE this call folds `text` in -- so one extreme document
    /// cannot dilute the very mean/stddev used to judge it -- and can only
    /// ever return `true` once both baselines have already seen at least
    /// [`MIN_SAMPLES`] prior documents.
    pub(crate) fn observe(&mut self, text: &str) -> bool {
        let compression_ratio = compression_ratio(text);
        let entropy = shannon_entropy(text);

        let compression_before = self.compression;
        let entropy_before = self.entropy;

        self.compression.update(compression_ratio);
        self.entropy.update(entropy);

        if compression_before.count() < MIN_SAMPLES || entropy_before.count() < MIN_SAMPLES {
            return false;
        }

        let compression_low = compression_before
            .deviations_below_mean(compression_ratio)
            .is_some_and(|z| z > REJECTION_STDDEVS);
        let entropy_low = entropy_before
            .deviations_below_mean(entropy)
            .is_some_and(|z| z > REJECTION_STDDEVS);

        if compression_low && entropy_low {
            debug!(
                compression_ratio,
                compression_mean = compression_before.mean(),
                compression_stddev = ?compression_before.stddev(),
                entropy,
                entropy_mean = entropy_before.mean(),
                entropy_stddev = ?entropy_before.stddev(),
                "pre-indexing quality gate (#1323) rejected a genuinely degenerate candidate: \
                 both compression ratio and Shannon entropy are more than \
                 {REJECTION_STDDEVS} standard deviations below their running mean"
            );
            return true;
        }

        false
    }
}

/// DEFLATE-compressed length over raw byte length, at max compression level.
/// Low means redundant/patterned; close to 1.0 means already dense.
///
/// No tokenizer involved -- this signal is purely about byte-level
/// redundancy, independent of [`shannon_entropy`]'s vocabulary-based one.
fn compression_ratio(text: &str) -> f64 {
    if text.is_empty() {
        return 1.0;
    }
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(text.as_bytes())
        .expect("compressing an in-memory buffer cannot fail");
    let compressed = encoder
        .finish()
        .expect("finishing an in-memory buffer cannot fail");
    compressed.len() as f64 / text.len() as f64
}

/// Order-0 Shannon entropy (bits per token) over token frequency, using
/// [`HashedNgramEmbedding::tokenize`] -- the same tokenizer the crate's
/// TF-IDF embedding uses, so this signal and the embedding agree on what a
/// "term" is. `0.0` for text with no tokens at all (nothing to be uncertain
/// about) rather than an undefined value.
fn shannon_entropy(text: &str) -> f64 {
    let tokens = HashedNgramEmbedding::tokenize(text);
    if tokens.is_empty() {
        return 0.0;
    }

    let mut counts: HashMap<&str, u64> = HashMap::new();
    for token in &tokens {
        *counts.entry(token.as_str()).or_insert(0) += 1;
    }

    let total = tokens.len() as f64;
    -counts
        .values()
        .map(|&count| {
            let p = count as f64 / total;
            p * p.log2()
        })
        .sum::<f64>()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic, varied natural-language sentence, parameterized by an
    /// index so a run of them differ from each other (distinct vocabulary,
    /// distinct length) -- real conversational memory content, not
    /// degenerate, and not identical from sample to sample (identical
    /// samples would drive a metric's stddev to exactly 0.0, which
    /// `WelfordStats::deviations_below_mean` deliberately treats as "no
    /// baseline to compare against" rather than manufacturing an infinite
    /// z-score).
    fn natural_document(i: usize) -> String {
        format!(
            "During session {i} we discussed the routing tree's dual-insert \
             threshold, reviewed the hashed n-gram embedding's document \
             frequency table, and decided the migration away from ONNX \
             should land before the next release candidate ships."
        )
    }

    fn seed_baseline(gate: &mut DegenerateContentGate, n: usize) {
        for i in 0..n {
            let rejected = gate.observe(&natural_document(i));
            assert!(
                !rejected,
                "natural document {i} must not be rejected while seeding the baseline"
            );
        }
    }

    #[test]
    fn test_gate_stays_permissive_below_minimum_sample_count() {
        let mut gate = DegenerateContentGate::new();
        // One clearly degenerate document, repeated MIN_SAMPLES - 1 times:
        // if the sample-count guard were not in effect, the very act of
        // repeating it would build a tight, low baseline and the gate would
        // start rejecting its own repeats well before this loop ends.
        let degenerate = "spam ".repeat(200);

        for i in 0..(MIN_SAMPLES - 1) {
            let rejected = gate.observe(&degenerate);
            assert!(
                !rejected,
                "observation {i} (of {} total, below MIN_SAMPLES={MIN_SAMPLES}) must not be \
                 rejected regardless of content: compression_before.count()={}, \
                 entropy_before.count()={}",
                MIN_SAMPLES - 1,
                i,
                i
            );
        }
    }

    #[test]
    fn test_genuinely_degenerate_content_is_rejected_once_enough_samples_exist() {
        let mut gate = DegenerateContentGate::new();
        seed_baseline(&mut gate, MIN_SAMPLES as usize + 5);

        // Single repeated token: compresses to a tiny fraction of its raw
        // size AND has exactly one distinct token, so its order-0 Shannon
        // entropy is 0.0 -- both signals are as low as they can be.
        let degenerate = "aaaa ".repeat(200);
        let rejected = gate.observe(&degenerate);

        assert!(
            rejected,
            "a single-repeated-token document must be rejected once the baseline has \
             {} prior samples: compression_ratio={}, entropy={}, \
             compression_stats={:?}, entropy_stats={:?}",
            MIN_SAMPLES + 5,
            compression_ratio(&degenerate),
            shannon_entropy(&degenerate),
            gate.compression,
            gate.entropy,
        );
    }

    #[test]
    fn test_templated_but_varied_content_is_not_rejected() {
        let mut gate = DegenerateContentGate::new();
        seed_baseline(&mut gate, MIN_SAMPLES as usize + 5);

        // Repeated STRUCTURE ("field_N: value") with a distinct field name
        // and a distinct value on every line -- low compression ratio (the
        // "field_N: " shape repeats), but high vocabulary variety, so
        // entropy stays comparable to ordinary prose rather than collapsing
        // toward 0 like the single-token case above.
        let templated: String = (0..80).map(|i| format!("field_{i}: value_{i}\n")).collect();
        let rejected = gate.observe(&templated);

        assert!(
            !rejected,
            "a templated-but-varied document (repeated structure, distinct vocabulary each \
             line) must NOT be rejected by the low-compression+low-entropy AND-gate: \
             compression_ratio={}, entropy={}, compression_stats={:?}, entropy_stats={:?}",
            compression_ratio(&templated),
            shannon_entropy(&templated),
            gate.compression,
            gate.entropy,
        );
    }

    #[test]
    fn test_compression_ratio_of_empty_text_is_one() {
        assert_eq!(compression_ratio(""), 1.0);
    }

    #[test]
    fn test_shannon_entropy_of_single_repeated_token_is_zero() {
        assert_eq!(
            shannon_entropy(&"aaaa ".repeat(50)),
            0.0,
            "a single distinct token has probability 1.0 and therefore zero entropy"
        );
    }

    #[test]
    fn test_shannon_entropy_of_all_distinct_tokens_is_log2_of_count() {
        let text: String = (0..16).map(|i| format!("word{i} ")).collect();
        let entropy = shannon_entropy(&text);
        // 16 equally-likely distinct tokens: entropy is exactly log2(16) = 4.0 bits.
        assert!(
            (entropy - 4.0).abs() < 1e-9,
            "16 equally-frequent distinct tokens must have entropy log2(16)=4.0, got {entropy}"
        );
    }
}
