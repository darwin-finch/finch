//! Retrieval-time gate deciding whether a recalled memory's text is injected
//! raw or as an extractive summary.
//!
//! Runs at retrieval time, not insert time (deliberately -- storage always
//! keeps the full text, so this gate's method can change without touching
//! anything durable). A compressed-size ratio is the trigger: it approximates
//! how redundant/patterned the text is -- a long diff, a repeated log
//! fragment, boilerplate -- far more cheaply than any embedding-based
//! measure, since it needs no tokenizer or model call, just the text itself.
//! It answers "is this worth summarizing", not "how" -- the actual reduction
//! is a deterministic extractive pick, never another LLM call.
//!
//! Every threshold here is a first guess, not a calibrated constant: real
//! calibration needs real recalled turns, which only exists once this is
//! wired into the REPL and being used (tracked as part of #8, "automatic
//! memory retrieval + injection").

use std::io::Write;

use rust_stemmers::{Algorithm, Stemmer};

/// Below this length, running the gate at all is noise: short strings
/// compress poorly regardless of redundancy (DEFLATE's own header/table
/// overhead dominates), and there is nothing worth trimming from a memory
/// this short in the first place.
const MIN_LENGTH_FOR_GATE: usize = 400;

/// Below this compressed/raw ratio, the text is redundant enough to be worth
/// summarizing. Chosen as "compresses to less than half its size" -- a
/// round, conservative starting point, not a measured value.
const COMPRESSION_RATIO_THRESHOLD: f64 = 0.5;

/// How many sentences an extractive summary keeps.
const SUMMARY_SENTENCE_COUNT: usize = 3;

/// How a recalled memory's text was decided to be presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecallPresentation {
    /// Short, or not redundant enough, to be worth summarizing -- sent as
    /// stored.
    Raw(String),
    /// Compressible enough that an extractive summary was substituted.
    /// `raw_len` is the original length, kept so the caller can disclose
    /// what was trimmed (e.g. in the collapsed/expanded recall row) without
    /// re-fetching the original text.
    Summarized { summary: String, raw_len: usize },
}

impl RecallPresentation {
    /// The text to actually inject into the request, regardless of which
    /// variant this is.
    pub fn text(&self) -> &str {
        match self {
            RecallPresentation::Raw(text) => text,
            RecallPresentation::Summarized { summary, .. } => summary,
        }
    }

    pub fn was_summarized(&self) -> bool {
        matches!(self, RecallPresentation::Summarized { .. })
    }
}

/// Decide, and if needed produce, how `text` should be presented.
pub fn present(text: &str) -> RecallPresentation {
    if text.len() < MIN_LENGTH_FOR_GATE {
        return RecallPresentation::Raw(text.to_string());
    }
    if compression_ratio(text) >= COMPRESSION_RATIO_THRESHOLD {
        return RecallPresentation::Raw(text.to_string());
    }
    RecallPresentation::Summarized {
        summary: extractive_summary(text, SUMMARY_SENTENCE_COUNT),
        raw_len: text.len(),
    }
}

/// DEFLATE-compressed length over raw length -- low means redundant/patterned,
/// close to 1.0 means already dense/incompressible.
fn compression_ratio(text: &str) -> f64 {
    if text.is_empty() {
        return 1.0;
    }
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(text.as_bytes())
        .expect("compressing an in-memory buffer cannot fail");
    let compressed = encoder
        .finish()
        .expect("finishing an in-memory buffer cannot fail");
    compressed.len() as f64 / text.len() as f64
}

/// Split `text` into naive sentences: break after `.`/`!`/`?` followed by
/// whitespace or end of string. Good enough for recalled conversational
/// text and diffs/logs (which this gate mostly triggers on); not a real NLP
/// sentence boundary detector.
fn split_sentences(text: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'.' || c == b'!' || c == b'?' {
            let mut end = i + 1;
            while end < bytes.len() && bytes[end].is_ascii_whitespace() {
                end += 1;
            }
            let sentence = text[start..i + 1].trim();
            if !sentence.is_empty() {
                sentences.push(sentence);
            }
            start = end;
            i = end;
            continue;
        }
        i += 1;
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        sentences.push(tail);
    }
    sentences
}

/// Lowercase, alphanumeric-only tokens, stemmed so "running"/"runs"/"run"
/// score as the same term.
fn stemmed_tokens(stemmer: &Stemmer, sentence: &str) -> Vec<String> {
    sentence
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(|w| stemmer.stem(w).into_owned())
        .collect()
}

/// Rank sentences by TF-ISF (term frequency within the sentence, weighted by
/// inverse sentence frequency across `text`'s own sentences -- extractive
/// summarization's usual document-free substitute for real IDF, which needs
/// a corpus this gate does not have), keep the top `keep`, and re-emit them
/// in original order so the result still reads as prose.
pub fn extractive_summary(text: &str, keep: usize) -> String {
    let sentences = split_sentences(text);
    if sentences.len() <= keep {
        return text.to_string();
    }

    let stemmer = Stemmer::create(Algorithm::English);
    let tokenized: Vec<Vec<String>> = sentences
        .iter()
        .map(|s| stemmed_tokens(&stemmer, s))
        .collect();

    let sentence_count = sentences.len() as f64;
    let mut doc_freq: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for tokens in &tokenized {
        let mut seen = std::collections::HashSet::new();
        for token in tokens {
            if seen.insert(token.as_str()) {
                *doc_freq.entry(token.as_str()).or_insert(0) += 1;
            }
        }
    }

    let scores: Vec<f64> = tokenized
        .iter()
        .map(|tokens| {
            if tokens.is_empty() {
                return 0.0;
            }
            let mut term_freq: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for token in tokens {
                *term_freq.entry(token.as_str()).or_insert(0) += 1;
            }
            let raw: f64 = term_freq
                .iter()
                .map(|(term, tf)| {
                    let df = *doc_freq.get(term).unwrap_or(&1) as f64;
                    let isf = (sentence_count / df).ln().max(0.0);
                    *tf as f64 * isf
                })
                .sum();
            // Mild length discount so the score rewards distinctive terms,
            // not just sentence length, without fully penalizing longer
            // sentences that genuinely carry more content.
            raw / (tokens.len() as f64).sqrt()
        })
        .collect();

    let mut ranked: Vec<usize> = (0..sentences.len()).collect();
    ranked.sort_by(|&a, &b| scores[b].partial_cmp(&scores[a]).unwrap());
    let mut kept: Vec<usize> = ranked.into_iter().take(keep).collect();
    kept.sort_unstable();

    kept.into_iter()
        .map(|i| sentences[i])
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_text_is_never_summarized() {
        let text = "The deploy key lives in 1Password.";
        assert_eq!(
            present(text),
            RecallPresentation::Raw(text.to_string()),
            "text under the length floor must skip the gate entirely, \
             not just fail the ratio check"
        );
    }

    #[test]
    fn test_highly_redundant_text_is_summarized() {
        let line =
            "Retry attempt failed with a transient network error, backing off and retrying. ";
        let text = line.repeat(30);
        assert!(
            text.len() >= MIN_LENGTH_FOR_GATE,
            "fixture must actually clear the length floor, or this test \
             cannot fail for the reason it exists: len={}",
            text.len()
        );
        let outcome = present(&text);
        assert!(
            outcome.was_summarized(),
            "30 repeats of one line must compress well past the threshold, \
             but was sent raw: ratio={}",
            compression_ratio(&text)
        );
    }

    #[test]
    fn test_dense_unique_text_is_not_summarized() {
        // Every sentence introduces a distinct fact -- no repeated
        // structure for DEFLATE to exploit, unlike the redundant fixture
        // above.
        let text = "The staging deployment token rotates every 90 days. \
             The billing webhook secret is stored in the ops vault under a \
             different key than the API credentials. Database migrations \
             run through a separate CI job gated on a manual approval from \
             the on-call engineer. The load balancer health check hits \
             /healthz every 5 seconds with a 2 second timeout. Redis \
             eviction policy is allkeys-lru with a 4GB memory cap on the \
             session cache instance.";
        assert!(
            text.len() >= MIN_LENGTH_FOR_GATE,
            "fixture must actually clear the length floor: len={}",
            text.len()
        );
        let outcome = present(text);
        assert!(
            !outcome.was_summarized(),
            "dense, non-redundant text must be sent raw, not lossily \
             trimmed: ratio={}",
            compression_ratio(text)
        );
    }

    #[test]
    fn test_summarized_presentation_keeps_the_raw_length() {
        let line = "Build failed: dependency resolution timed out after 30 seconds. ";
        let text = line.repeat(20);
        match present(&text) {
            RecallPresentation::Summarized { raw_len, .. } => {
                assert_eq!(
                    raw_len,
                    text.len(),
                    "raw_len must be the ORIGINAL length, not the summary's, \
                     so the UI can disclose what was actually trimmed"
                );
            }
            other => {
                panic!("fixture must summarize for this assertion to mean anything: {other:?}")
            }
        }
    }

    #[test]
    fn test_extractive_summary_keeps_requested_sentence_count() {
        let text = "Alpha sentence about deployment keys and secrets. \
             Beta sentence about database migration scheduling. \
             Gamma sentence about load balancer health checks. \
             Delta sentence about redis cache eviction policy. \
             Epsilon sentence about billing webhook configuration.";
        let summary = extractive_summary(text, 2);
        let kept = split_sentences(&summary);
        assert_eq!(
            kept.len(),
            2,
            "must keep exactly the requested sentence count, got {kept:?}"
        );
    }

    #[test]
    fn test_extractive_summary_preserves_original_order() {
        let text = "First sentence mentions rust programming lifetimes ownership borrowing. \
             Second short one. \
             Third sentence mentions rust programming lifetimes ownership borrowing again here. \
             Fourth short one too.";
        let summary = extractive_summary(text, 2);
        // The two "rust programming" sentences should win on distinctive
        // term repetition across the excerpt; whichever two are kept, their
        // relative order in the summary must match their order in `text`.
        let first_pos = text.find("First").unwrap();
        let third_pos = text.find("Third").unwrap();
        assert!(first_pos < third_pos, "fixture sanity check");
        let summary_first = summary.find("First");
        let summary_third = summary.find("Third");
        if let (Some(sf), Some(st)) = (summary_first, summary_third) {
            assert!(
                sf < st,
                "kept sentences must stay in original document order: {summary:?}"
            );
        }
    }

    #[test]
    fn test_extractive_summary_no_op_when_already_short_enough() {
        let text = "One. Two. Three.";
        assert_eq!(
            extractive_summary(text, 5),
            text,
            "asking to keep more sentences than exist must return the \
             original text unchanged, not pad or panic"
        );
    }

    #[test]
    fn test_compression_ratio_of_empty_text_is_one() {
        assert_eq!(compression_ratio(""), 1.0);
    }
}
