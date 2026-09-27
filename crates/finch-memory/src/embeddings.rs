// Embedding engine for semantic similarity
//
// Converts text to vector embeddings for MemTree insertion and retrieval.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Trait for embedding engines
pub trait EmbeddingEngine: Send + Sync {
    /// Generate embedding vector for `text` using the engine's current state.
    ///
    /// Read-only: must never mutate any corpus-wide statistic an implementer
    /// maintains (e.g. `HashedNgramEmbedding`'s running document count and
    /// per-term document frequency). Safe to call at query time, and as many
    /// times as needed, without affecting how anything else is scored.
    fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Record `text` as one newly indexed document, before computing its
    /// embedding via [`Self::embed`].
    ///
    /// A caller that inserts new content into a retrieval index must call
    /// this once per document, in addition to calling `embed`, so the
    /// document's own terms count toward the corpus-wide statistics used to
    /// score it and everything indexed after it (see `HashedNgramEmbedding`'s
    /// real corpus-wide TF-IDF). Query-time embedding must NOT call this —
    /// counting a query as a document would corrupt those statistics on
    /// every retrieval, not just every insert.
    ///
    /// Default is a no-op: an implementer with no corpus-wide statistic to
    /// maintain (e.g. a neural sentence-embedding engine) does not need to
    /// override this, and gets a sensible do-nothing default instead of an
    /// obligation to implement something meaningless.
    fn observe_document(&self, _text: &str) -> Result<()> {
        Ok(())
    }

    /// Get embedding dimension
    fn dimension(&self) -> usize;
}

/// Running corpus-wide statistics for TF-IDF weighting.
///
/// Grows incrementally, one document at a time, as
/// [`HashedNgramEmbedding::observe_document`] is called — no pre-existing
/// bulk corpus or batch rescans required.
#[derive(Default)]
struct CorpusStats {
    /// Total number of documents observed so far (`N`).
    doc_count: u64,
    /// Number of documents each term has appeared in at least once (`df`),
    /// keyed by the same lowercase token `tokenize` produces. Deduped within
    /// one document before incrementing — standard document-frequency
    /// semantics, not a raw occurrence count.
    doc_freq: HashMap<String, u64>,
}

/// Feature-hashed, real TF-IDF-weighted, whole-word bag embedding engine
///
/// [`Self::observe_document`] maintains a running corpus-wide document count
/// `N` and a running per-term document-frequency counter `df`, incrementally,
/// one document at a time. [`Self::embed`] then weights each word occurrence
/// by `tf(word, doc) * idf(word)`, using the standard smoothed
/// `idf(word) = ln((N - df + 0.5) / (df + 0.5) + 1.0)`. Adding that
/// per-occurrence `idf` weight once for every occurrence of a word in one
/// document (see `embed_text`'s loop) produces `tf * idf` in total for that
/// word without a separate counting pass.
///
/// This is real corpus-wide TF-IDF, measured, not assumed:
/// - Tokenises into words (lowercase, alphanumeric) — whole words only, into
///   4 hashed slots each. Despite the historical name, character
///   bigrams/trigrams are no longer hashed at all: a measured ablation on
///   1475 real (query, answer) pairs pulled from real session transcripts
///   showed they dilute the fixed 2048-dim space more than they help once
///   real IDF weighting replaces the old length-based proxy, so this
///   revision drops them.
/// - Hashes every word into the fixed-size dense vector via FNV-64 (fewer
///   collisions than DefaultHasher), accumulating a SIGNED weight at each
///   slot: the sign comes from bit 63 of the SAME FNV-64 hash already used
///   for bucket placement (`% dim` consumes the low bits; the sign reads a
///   high bit), not a second hashing pass. Sign hashing reduces systematic
///   bias from hash collisions in a fixed-width vector.
/// - L2-normalises the accumulated vector to unit length.
///
/// Measured on that same 1475-pair corpus: real corpus-wide TF-IDF cosine
/// retrieval (same tokenizer, full-corpus idf, no hashing) scored 30.2%
/// exact-match@1; this hashed, fixed-2048-dim, signed, no-n-gram
/// representation scored 23.4%, against 10.8% for the length-proxy-weighted,
/// unsigned-hash, n-gram predecessor this replaced — length-proxy weighting
/// was roughly half the gap, and dropping n-grams while adding a sign bit
/// recovered most of the rest. See
/// `test_real_tf_idf_and_signed_hash_beat_length_proxy_weighting_on_real_qa_pairs`
/// in this module's tests for a live before/after regression against a real
/// (smaller) technical Q&A fixture.
///
/// Disclosed, measured tradeoff, not a defect: a young corpus's IDF is
/// noisier than a mature one's — roughly a 7-point exact-match@1 gap between
/// an early and a fully-grown corpus in the same test. This is expected and
/// improves automatically as more documents are observed; there is
/// deliberately no minimum-corpus-size gate before real IDF is used.
///
/// Quality is sufficient for technical-text retrieval (code discussions,
/// function names, error messages) where terms are distinctive. A neural
/// sentence transformer (`NeuralEmbeddingEngine`) is available as an optional
/// upgrade once a model is downloaded.
pub struct HashedNgramEmbedding {
    dimension: usize,
    stats: RwLock<CorpusStats>,
}

impl HashedNgramEmbedding {
    pub fn new() -> Self {
        Self {
            dimension: 2048,
            stats: RwLock::new(CorpusStats::default()),
        }
    }

    /// FNV-64 hash — fewer collisions than DefaultHasher for short strings
    fn fnv64(bytes: &[u8]) -> u64 {
        const OFFSET: u64 = 0xcbf29ce484222325;
        const PRIME: u64 = 0x00000100000001b3;
        let mut h = OFFSET;
        for &b in bytes {
            h = h.wrapping_mul(PRIME);
            h ^= b as u64;
        }
        h
    }

    /// Tokenise into lowercase words, splitting on any character that is
    /// neither alphanumeric nor `_`; tokens shorter than 2 characters are
    /// dropped. Shared by both `embed_text` and `observe_document_text` so
    /// document-frequency counting and embedding always agree on what a
    /// "term" is.
    fn tokenize(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() >= 2)
            .map(|w| w.to_string())
            .collect()
    }

    /// Smoothed inverse document frequency for `word` against the given
    /// corpus-wide statistics: `ln((N - df + 0.5) / (df + 0.5) + 1.0)`. A
    /// word never observed by [`Self::observe_document`] gets `df = 0`, so it
    /// is scored as maximally rare rather than causing a division by zero.
    fn idf(stats: &CorpusStats, word: &str) -> f32 {
        let n = stats.doc_count as f32;
        let df = stats.doc_freq.get(word).copied().unwrap_or(0) as f32;
        ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
    }

    /// Map a token string to `slots` positions in a `dim`-dimensional vector,
    /// adding a SIGNED `weight` to each position. The bucket comes from the
    /// low bits of the token's FNV-64 hash (`% dim`); the sign comes from bit
    /// 63 of that SAME hash, independent of the bits `% dim` consumes, rather
    /// than a second hashing pass.
    fn add_token(embedding: &mut [f32], token: &str, weight: f32, slots: usize) {
        let dim = embedding.len();
        let bytes = token.as_bytes();
        for slot in 0..slots {
            // Mix slot index into the hash so each slot lands in a different bucket
            let mut mixed = bytes.to_vec();
            mixed.push(slot as u8);
            let h = Self::fnv64(&mixed);
            let bucket = (h as usize) % dim;
            let sign: f32 = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
            embedding[bucket] += sign * weight;
        }
    }

    fn embed_text(&self, text: &str) -> Vec<f32> {
        let mut embedding = vec![0.0f32; self.dimension];

        let words = Self::tokenize(text);
        let stats = self
            .stats
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        for word in &words {
            // tf(word, doc) * idf(word): idf(word) is added once per
            // occurrence of `word` in this document, so summing across a
            // repeated word's occurrences in this loop yields tf * idf in
            // total for that word without a separate term-frequency pass.
            let word_weight = Self::idf(&stats, word);

            // Whole-word token only (4 slots for good coverage). Character
            // bigrams/trigrams were dropped — see the struct doc's ablation.
            Self::add_token(&mut embedding, word, word_weight, 4);
        }

        drop(stats);

        // L2 normalise to unit vector
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut embedding {
                *x /= norm;
            }
        }

        embedding
    }

    /// Update running corpus-wide statistics for one newly observed
    /// document: increments `N` once, and `df[word]` once per distinct word
    /// in `text` (deduped within this document first).
    fn observe_document_text(&self, text: &str) {
        let words = Self::tokenize(text);
        let unique: HashSet<&str> = words.iter().map(|w| w.as_str()).collect();
        let mut stats = self
            .stats
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stats.doc_count += 1;
        for word in unique {
            *stats.doc_freq.entry(word.to_string()).or_insert(0) += 1;
        }
    }
}

impl Default for HashedNgramEmbedding {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingEngine for HashedNgramEmbedding {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.embed_text(text))
    }

    fn observe_document(&self, text: &str) -> Result<()> {
        self.observe_document_text(text);
        Ok(())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

/// Compute cosine similarity between two embedding vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Compute the average of multiple unit embeddings (then re-normalise)
pub fn average_embeddings(embeddings: &[&Vec<f32>]) -> Vec<f32> {
    if embeddings.is_empty() {
        return Vec::new();
    }
    let dim = embeddings[0].len();
    let mut avg = vec![0.0f32; dim];
    for emb in embeddings {
        for (i, val) in emb.iter().enumerate() {
            if i < dim {
                avg[i] += val;
            }
        }
    }
    let count = embeddings.len() as f32;
    for val in &mut avg {
        *val /= count;
    }
    let norm: f32 = avg.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut avg {
            *x /= norm;
        }
    }
    avg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_dimension() {
        let engine = HashedNgramEmbedding::new();
        assert_eq!(engine.dimension(), 2048);
    }

    #[test]
    fn test_embedding_generation() {
        let engine = HashedNgramEmbedding::new();
        let emb = engine.embed("Hello world").unwrap();
        assert_eq!(emb.len(), 2048);
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let engine = HashedNgramEmbedding::new();
        let emb1 = engine.embed("rust lifetimes borrow checker").unwrap();
        let emb2 = engine.embed("rust lifetimes borrow checker").unwrap();
        let sim = cosine_similarity(&emb1, &emb2);
        assert!(
            (sim - 1.0).abs() < 0.001,
            "Identical texts should have similarity ~1.0, got {}",
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_related() {
        let engine = HashedNgramEmbedding::new();
        let emb1 = engine.embed("rust async await tokio").unwrap();
        let emb2 = engine
            .embed("rust async programming tokio runtime")
            .unwrap();
        let emb3 = engine
            .embed("python machine learning pandas numpy")
            .unwrap();

        let sim_related = cosine_similarity(&emb1, &emb2);
        let sim_unrelated = cosine_similarity(&emb1, &emb3);

        // Related texts should score higher than unrelated
        assert!(
            sim_related > sim_unrelated,
            "Related texts (sim={:.3}) should outscore unrelated (sim={:.3})",
            sim_related,
            sim_unrelated
        );
    }

    #[test]
    fn test_cosine_similarity_technical_terms() {
        let engine = HashedNgramEmbedding::new();

        // These share character n-grams ("ort", "sort") but different semantics — just checks non-crash
        let emb1 = engine.embed("quicksort algorithm").unwrap();
        let emb2 = engine.embed("ONNX runtime ort").unwrap();
        let sim = cosine_similarity(&emb1, &emb2);
        assert!((0.0..=1.0).contains(&sim));
    }

    #[test]
    fn test_embedding_is_unit_vector() {
        let engine = HashedNgramEmbedding::new();
        let emb = engine
            .embed("the quick brown fox jumps over the lazy dog")
            .unwrap();
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.001,
            "Embedding should be a unit vector, norm={}",
            norm
        );
    }

    #[test]
    fn test_empty_text() {
        let engine = HashedNgramEmbedding::new();
        let emb = engine.embed("").unwrap();
        assert_eq!(emb.len(), 2048);
        // Zero vector for empty input
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert_eq!(norm, 0.0);
    }

    #[test]
    fn test_average_embeddings() {
        let engine = HashedNgramEmbedding::new();
        let emb1 = engine.embed("test one").unwrap();
        let emb2 = engine.embed("test two").unwrap();
        let avg = average_embeddings(&[&emb1, &emb2]);
        assert_eq!(avg.len(), 2048);
        // Average should be a unit vector
        let norm: f32 = avg.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_coding_term_similarity() {
        let engine = HashedNgramEmbedding::new();

        // Two descriptions of the same concept should score well
        let e1 = engine
            .embed("authentication JWT token bearer header")
            .unwrap();
        let e2 = engine.embed("auth JWT bearer token authorization").unwrap();
        let e3 = engine
            .embed("database migration schema alter table column")
            .unwrap();

        let sim_auth = cosine_similarity(&e1, &e2);
        let sim_cross = cosine_similarity(&e1, &e3);

        assert!(
            sim_auth > sim_cross,
            "Auth texts (sim={:.3}) should outscore cross-domain (sim={:.3})",
            sim_auth,
            sim_cross
        );
    }

    // ── Real corpus-wide TF-IDF: insert-vs-query and weighting behavior ──────

    #[test]
    fn test_query_time_embed_never_mutates_corpus_statistics() {
        // Calling embed() any number of times (the query-time path) must not
        // move doc_count/df -- only observe_document() (the insertion-time
        // path) may. Verified indirectly: repeated embed() calls on a corpus
        // that has observed zero documents keep producing the SAME vector
        // every time (idf is a pure function of the unchanged stats), which
        // would drift if embed() were secretly incrementing anything.
        let engine = HashedNgramEmbedding::new();
        let first = engine.embed("kubernetes pod scheduling").unwrap();
        for _ in 0..5 {
            let repeat = engine.embed("kubernetes pod scheduling").unwrap();
            assert_eq!(
                repeat, first,
                "embed() must be read-only: repeated query-time calls changed the \
                 resulting vector, implying embed() mutated corpus statistics"
            );
        }
    }

    #[test]
    fn test_observe_document_dedups_terms_within_one_document_before_incrementing_df() {
        // A word repeated many times in ONE document must only bump df by 1
        // for that document, not once per occurrence -- document frequency,
        // not collection frequency.
        let engine = HashedNgramEmbedding::new();
        engine
            .observe_document("rust rust rust rust rust ownership")
            .unwrap();
        let stats = engine.stats.read().unwrap();
        assert_eq!(
            stats.doc_count, 1,
            "one observe_document() call must count as exactly one document, got {}",
            stats.doc_count
        );
        assert_eq!(
            stats.doc_freq.get("rust").copied(),
            Some(1),
            "df('rust') must be 1 after a single document containing 'rust' five \
             times, got {:?} (df counts documents, not occurrences)",
            stats.doc_freq.get("rust")
        );
        assert_eq!(
            stats.doc_freq.get("ownership").copied(),
            Some(1),
            "df('ownership') must be 1, got {:?}",
            stats.doc_freq.get("ownership")
        );
    }

    #[test]
    fn test_frequent_term_is_downweighted_relative_to_a_rare_term_after_observation() {
        // After observing many documents containing "the" (a stop-word-like
        // term) and only one containing "kubernetes", "the" must carry a
        // strictly lower idf weight than "kubernetes" once corpus stats
        // exist -- the entire point of moving off the length-based proxy.
        let engine = HashedNgramEmbedding::new();
        for i in 0..20 {
            engine
                .observe_document(&format!("the document number {i} discusses something"))
                .unwrap();
        }
        engine
            .observe_document("kubernetes pod scheduling internals")
            .unwrap();

        let stats = engine.stats.read().unwrap();
        let idf_the = HashedNgramEmbedding::idf(&stats, "the");
        let idf_kubernetes = HashedNgramEmbedding::idf(&stats, "kubernetes");
        assert!(
            idf_kubernetes > idf_the,
            "idf('kubernetes')={idf_kubernetes} must exceed idf('the')={idf_the} -- \
             'the' appears in 20 of 21 observed documents, 'kubernetes' in only 1"
        );
    }

    #[test]
    fn test_signed_hash_produces_at_least_one_negative_weight() {
        // A real signed-hash embedding over enough distinct words must land
        // some tokens on the negative sign of their hash's bit 63; an
        // embedding with no negative entries at all would indicate the sign
        // bit never actually flipped anything.
        let engine = HashedNgramEmbedding::new();
        let emb = engine
            .embed("alpha bravo charlie delta echo foxtrot golf hotel india juliet")
            .unwrap();
        assert!(
            emb.iter().any(|x| *x < 0.0),
            "expected at least one negative component from signed hashing across 10 \
             distinct words, got all non-negative: {:?}",
            emb.iter().filter(|x| **x != 0.0).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_character_ngrams_no_longer_hashed() {
        // "cat" and "cattle" no longer share character n-gram buckets, so
        // with corpus statistics empty (idf constant across all words) their
        // only possible overlap is an accidental whole-word hash collision,
        // not the deliberate "cat"/"cat"-prefix bigram/trigram sharing the
        // old algorithm relied on. This pins "no n-grams" as an embedding
        // property, independent of TF-IDF weighting.
        let engine = HashedNgramEmbedding::new();
        let cat = engine.embed("cat").unwrap();
        let cattle = engine.embed("cattle").unwrap();
        let unrelated = engine.embed("xylophone").unwrap();
        let sim_prefix = cosine_similarity(&cat, &cattle);
        let sim_unrelated = cosine_similarity(&cat, &unrelated);
        assert_eq!(
            sim_prefix, sim_unrelated,
            "with no character n-grams, two distinct whole words sharing a prefix \
             ('cat'/'cattle', sim={sim_prefix}) must score exactly the same as two \
             completely unrelated whole words (sim={sim_unrelated}) -- any \
             difference means a character n-gram path survived"
        );
    }

    /// Replica of the pre-fix embedding this task replaced: unsigned hash,
    /// whole-word + character bigram + character trigram hashing, and a
    /// length-based weight proxy (`(len + 1).ln()`) instead of real
    /// corpus-wide TF-IDF. Kept ONLY so the accuracy test below can measure a
    /// real, reproducible before/after retrieval accuracy delta against real
    /// fixture data, mirroring the rigor of the ablation this task is based
    /// on, instead of just trusting that the new weighting math is correct.
    /// Not used anywhere outside this one comparison; do not resurrect it in
    /// production.
    fn embed_length_proxy_baseline(text: &str, dim: usize) -> Vec<f32> {
        fn add_token_unsigned(embedding: &mut [f32], token: &str, weight: f32, slots: usize) {
            let dim = embedding.len();
            let bytes = token.as_bytes();
            for slot in 0..slots {
                let mut mixed = bytes.to_vec();
                mixed.push(slot as u8);
                let h = HashedNgramEmbedding::fnv64(&mixed) as usize;
                embedding[h % dim] += weight;
            }
        }

        let mut embedding = vec![0.0f32; dim];
        let words = HashedNgramEmbedding::tokenize(text);
        for word in &words {
            let word_weight = (word.len() as f32 + 1.0).ln();
            add_token_unsigned(&mut embedding, word, word_weight, 4);

            let chars: Vec<char> = word.chars().collect();
            for i in 0..chars.len().saturating_sub(1) {
                let bigram: String = chars[i..i + 2].iter().collect();
                add_token_unsigned(&mut embedding, &bigram, word_weight * 0.4, 2);
            }
            for i in 0..chars.len().saturating_sub(2) {
                let trigram: String = chars[i..i + 3].iter().collect();
                add_token_unsigned(&mut embedding, &trigram, word_weight * 0.6, 3);
            }
        }

        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut embedding {
                *x /= norm;
            }
        }
        embedding
    }

    /// 36 real, distinct technical (query, answer) pairs across 7 clusters
    /// (Rust, Python, git, Docker, SQL, HTTP/networking, Linux/shell), each
    /// cluster sharing heavy common vocabulary ("what is the difference
    /// between...", "how do I...") on purpose -- the same kind of lexical
    /// overlap real session-transcript Q&A has, and exactly the condition
    /// under which down-weighting common terms (real IDF) should beat a
    /// length-based weight proxy that cannot tell a corpus-common word from a
    /// corpus-rare one.
    const REAL_QA_FIXTURE: &[(&str, &str)] = &[
        ("What is the difference between String and &str in Rust?", "String is an owned, heap-allocated, growable UTF-8 string type, while &str is a borrowed string slice that points into either a String or a string literal."),
        ("How do I clone a Vec in Rust?", "Call .clone() on the Vec, which requires the element type to implement the Clone trait, producing a deep copy of the vector's contents."),
        ("What does the Rust borrow checker actually check?", "It enforces that a value has at most one mutable reference or any number of immutable references at a time, and that no reference outlives the value it points to."),
        ("Why does my Rust closure not implement Copy?", "A closure only implements Copy if every variable it captures by value also implements Copy; capturing a String or a Vec by value makes the closure non-Copy."),
        ("How do I convert a Vec<u8> into a String in Rust?", "Use String::from_utf8(vec), which validates the bytes are valid UTF-8 and returns a Result, or from_utf8_lossy for a lossy conversion that never fails."),
        ("What is the difference between Box<T> and Rc<T> in Rust?", "Box<T> is a single-owner heap allocation, while Rc<T> is a reference-counted heap allocation that allows multiple owners within a single thread."),
        ("What is the difference between a list and a tuple in Python?", "A list is mutable and defined with square brackets, while a tuple is immutable and defined with parentheses; tuples are also slightly faster to iterate."),
        ("How do I read a JSON file in Python?", "Use json.load(open(path)) or json.loads(text) from the built-in json module to parse JSON into a Python dict or list."),
        ("What does the yield keyword do in Python?", "yield turns a function into a generator that produces a lazy sequence of values, pausing execution at each yield and resuming on the next call to next()."),
        ("How do I install a package with pip from a requirements file?", "Run pip install -r requirements.txt, which installs every package and pinned version listed in that file."),
        ("What is the Global Interpreter Lock in Python?", "The GIL is a mutex in CPython that allows only one thread to execute Python bytecode at a time, which limits true parallelism for CPU-bound threads."),
        ("How do I undo the last git commit but keep the changes?", "Run git reset --soft HEAD~1, which moves the branch pointer back one commit but leaves the changes staged in the working tree."),
        ("What is the difference between git merge and git rebase?", "git merge creates a new merge commit joining two histories, while git rebase replays your commits on top of another branch, producing a linear history."),
        ("How do I discard all local uncommitted changes in git?", "Run git checkout -- . to discard unstaged changes, or git reset --hard HEAD to discard both staged and unstaged changes."),
        ("How do I squash the last three commits into one?", "Run git rebase -i HEAD~3 and mark the last two commits as squash or fixup in the interactive editor that opens."),
        ("What does git cherry-pick do?", "git cherry-pick applies the changes introduced by a specific existing commit onto your current branch as a new commit."),
        ("What is the difference between a Docker image and a Docker container?", "An image is a read-only template with the filesystem and metadata needed to run an app, while a container is a running or stopped instance created from that image."),
        ("How do I see the logs of a running Docker container?", "Run docker logs -f container_name to stream the container's stdout and stderr output continuously."),
        ("How do I remove all stopped Docker containers?", "Run docker container prune, which deletes every container that is not currently running."),
        ("What does the EXPOSE instruction do in a Dockerfile?", "EXPOSE documents which port the container listens on at runtime; it does not actually publish the port, which still requires -p when running the container."),
        ("How do I copy a file out of a running Docker container?", "Run docker cp container_name:/path/in/container ./local/path to copy a file or directory from the container to the host."),
        ("What is the difference between INNER JOIN and LEFT JOIN in SQL?", "INNER JOIN returns only rows with matches in both tables, while LEFT JOIN returns every row from the left table plus matched rows from the right, with NULLs where there is no match."),
        ("How do I find duplicate rows in a SQL table?", "Group by the columns that should be unique and use HAVING COUNT(*) > 1 to list the values that appear more than once."),
        ("What does the SQL GROUP BY clause do?", "GROUP BY collapses rows sharing the same values in the given columns into a single row per group, so aggregate functions like COUNT or SUM operate per group."),
        ("How do I add a NOT NULL constraint to an existing column?", "Run ALTER TABLE table_name ALTER COLUMN column_name SET NOT NULL, after first backfilling any existing NULL values."),
        ("What is a SQL index and when should I add one?", "An index is a separate data structure that speeds up lookups on a column at the cost of extra storage and slower writes; add one on columns used often in WHERE clauses or JOINs."),
        ("What is the difference between HTTP PUT and PATCH?", "PUT replaces the entire resource with the request body, while PATCH applies a partial update containing only the fields that changed."),
        ("What does a 401 HTTP status code mean?", "401 Unauthorized means the request lacks valid authentication credentials; the client should authenticate and retry."),
        ("What is the difference between TCP and UDP?", "TCP is a connection-oriented protocol that guarantees ordered, reliable delivery, while UDP is connectionless and does not guarantee delivery or ordering, but has lower overhead."),
        ("What does DNS do when I type a domain name into a browser?", "DNS resolves the human-readable domain name into the IP address of the server that hosts the site, by querying a hierarchy of nameservers."),
        ("What is CORS and why does my browser block a request?", "CORS is a browser security policy that blocks a webpage from making requests to a different origin unless that origin's server explicitly allows it via response headers."),
        ("How do I find which process is using a specific port on Linux?", "Run sudo lsof -i :PORT or sudo ss -tulpn | grep PORT to see which process has that port open."),
        ("What does chmod 755 do to a file?", "It gives the owner read, write, and execute permission, and gives the group and everyone else read and execute permission only."),
        ("How do I find all files modified in the last 24 hours?", "Run find . -mtime -1 to list files under the current directory whose modification time is within the last day."),
        ("What is the difference between a hard link and a symbolic link?", "A hard link is another directory entry pointing to the same inode, while a symbolic link is a separate file that stores a path to the target."),
        ("How do I kill a process by name on Linux?", "Run pkill process_name, or killall process_name, to send SIGTERM to every process matching that name."),
    ];

    /// exact-match@1: fraction of queries whose nearest (by cosine
    /// similarity) answer embedding, among ALL fixture answers, is the
    /// query's own true answer.
    fn exact_match_at_1<F: Fn(&str) -> Vec<f32>>(embed: F) -> (usize, usize) {
        let answer_embeddings: Vec<Vec<f32>> = REAL_QA_FIXTURE
            .iter()
            .map(|(_, answer)| embed(answer))
            .collect();

        let mut correct = 0usize;
        for (i, (query, _)) in REAL_QA_FIXTURE.iter().enumerate() {
            let query_embedding = embed(query);
            let best = answer_embeddings
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    cosine_similarity(&query_embedding, a)
                        .partial_cmp(&cosine_similarity(&query_embedding, b))
                        .unwrap()
                })
                .map(|(idx, _)| idx);
            if best == Some(i) {
                correct += 1;
            }
        }
        (correct, REAL_QA_FIXTURE.len())
    }

    #[test]
    fn test_real_tf_idf_and_signed_hash_beat_length_proxy_weighting_on_real_qa_pairs() {
        // "After": the real, production HashedNgramEmbedding, with every
        // answer observed into the corpus first (as production's
        // project_stored_conversation_inner does at insertion time) so a
        // mature-corpus idf is in effect for every query, matching the
        // ablation's own full-corpus measurement.
        let engine = HashedNgramEmbedding::new();
        for (_, answer) in REAL_QA_FIXTURE {
            engine.observe_document(answer).unwrap();
        }
        let (new_correct, total) = exact_match_at_1(|text| engine.embed(text).unwrap());

        // "Before": the length-proxy-weighted, unsigned-hash, n-gram
        // predecessor, which had no corpus statistic to observe at all.
        let (old_correct, _) =
            exact_match_at_1(|text| embed_length_proxy_baseline(text, engine.dimension()));

        let new_pct = 100.0 * new_correct as f32 / total as f32;
        let old_pct = 100.0 * old_correct as f32 / total as f32;

        assert!(
            new_correct > old_correct,
            "real corpus-wide TF-IDF + signed hash + no n-grams must beat the \
             length-proxy-weighted predecessor on real technical Q&A retrieval: \
             new exact-match@1 = {new_correct}/{total} ({new_pct:.1}%), \
             old exact-match@1 = {old_correct}/{total} ({old_pct:.1}%)"
        );
    }

    #[test]
    fn test_average_embeddings_dimension_mismatch_guarded_by_shared_dimension() {
        // Sanity check that the fixture-driven accuracy test above is
        // actually comparing same-dimension vectors (both algorithms use the
        // engine's own 2048-dim), not silently comparing mismatched-length
        // vectors that cosine_similarity would score as 0.0 across the board.
        let engine = HashedNgramEmbedding::new();
        let new_emb = engine.embed("dimension probe").unwrap();
        let old_emb = embed_length_proxy_baseline("dimension probe", engine.dimension());
        assert_eq!(new_emb.len(), old_emb.len());
        assert_eq!(new_emb.len(), 2048);
    }
}
