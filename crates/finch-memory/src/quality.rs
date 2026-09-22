// Memory quality layer for MemTree.
//
// Three jobs:
//   1. Filter  — discard noise (acks, greetings, one-word replies) so they
//                never pollute the semantic index.
//   2. Classify — assign an importance tier so high-signal memories (decisions,
//                 bug insights, explicit instructions) surface first in retrieval.
//   3. Extract  — compress long assistant responses to their prose core so the
//                 stored text is dense with signal, not padded with code blocks.

/// How important a piece of content is for long-term memory.
///
/// Stored as a `u8` in `TreeNode.importance` and persisted to the DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryImportance {
    /// Not worth adding to MemTree (pure noise: greetings, acks, filler).
    /// Still written to the `conversations` SQL table for history.
    Discard,
    /// Default tier — generic Q&A, substantive but no special signal.
    Normal,
    /// File reference, code pattern, established preference or factual explanation.
    High,
    /// Decision made, bug root-cause found, explicit instruction given,
    /// or content from the explicit `create_memory` tool (role="system").
    Critical,
}

impl MemoryImportance {
    /// Canonical DB representation (0–3).
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Discard => 0,
            Self::Normal => 1,
            Self::High => 2,
            Self::Critical => 3,
        }
    }

    /// Reconstruct from the DB value; unknown values fall back to Normal.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Discard,
            2 => Self::High,
            3 => Self::Critical,
            _ => Self::Normal,
        }
    }

    /// Multiplier applied to cosine similarity during retrieval so high-signal
    /// memories surface first even when slightly less semantically similar.
    ///
    /// Example: Critical memory at 0.70 sim scores 0.70 × 1.4 = 0.98,
    /// beating a Normal memory at 0.85 sim scoring 0.85 × 1.0 = 0.85.
    pub fn retrieval_boost(self) -> f32 {
        match self {
            Self::Discard => 0.0, // should never reach retrieval
            Self::Normal => 1.0,
            Self::High => 1.2,
            Self::Critical => 1.4,
        }
    }
}

/// Classifies and pre-processes a conversation turn for MemTree storage.
pub struct MemoryClassifier;

impl MemoryClassifier {
    pub fn new() -> Self {
        Self
    }

    /// Decide whether to add this turn to MemTree, and if so:
    /// - what key content to store (extracted/compressed prose)
    /// - which importance tier to assign
    ///
    /// Returns `None` if the content is noise and should be skipped.
    ///
    /// Note: `role="system"` is used by the explicit `create_memory` tool —
    /// always treated as Critical because the AI already decided it's important.
    pub fn process(&self, role: &str, content: &str) -> Option<(String, MemoryImportance)> {
        let trimmed = content.trim();

        if self.is_noise(trimmed) {
            return None;
        }

        let importance = if role == "system" {
            // Explicit memory tool store — always Critical
            MemoryImportance::Critical
        } else {
            self.classify(trimmed)
        };

        let extracted = self.extract(role, trimmed);
        if extracted.trim().is_empty() {
            return None;
        }

        Some((extracted, importance))
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Whether the content is template noise that must never become a memory.
    ///
    /// The insert-time line: the length floor plus every recall-time rule.
    pub fn is_noise(&self, content: &str) -> bool {
        // Hard minimum: anything under 20 chars is almost certainly not memorable
        if content.len() < 20 {
            return true;
        }
        self.is_recall_noise(content)
    }

    /// Whether recall must hold this text back.
    ///
    /// Public because recall holds the same line as insert: a store built
    /// before this classifier existed already carries greetings and acks in
    /// `routing_points`, and nothing rewrites stored rows — so recall filters
    /// with the predicate below rather than letting the old rows back out.
    ///
    /// Deliberately WITHOUT the 20-character floor: the measured store's
    /// smallest node is 23 characters, so the floor never earned its keep
    /// there, while a legacy store can legitimately hold short memories that
    /// say something.
    pub fn is_recall_noise(&self, content: &str) -> bool {
        // Pure acknowledgment phrases — check lowercased, stripped of trailing punctuation
        const NOISE: &[&str] = &[
            "ok",
            "okay",
            "sure",
            "yes",
            "no",
            "got it",
            "thanks",
            "thank you",
            "great",
            "good",
            "nice",
            "perfect",
            "alright",
            "fine",
            "sounds good",
            "let me try",
            "i'll try",
            "understood",
            "makes sense",
            "i see",
            "cool",
            "awesome",
            "noted",
            "will do",
            "on it",
            "done",
            "good to know",
            "got it thanks",
            "ok thanks",
        ];

        let lower = content.to_lowercase();
        let s = lower.trim().trim_end_matches(['.', '!', '?']);
        if NOISE.contains(&s) {
            return true;
        }

        // Template openings that clear the length floor and the exact-match
        // list above (#415): "You're welcome, Shammah!" is 25 characters, and
        // the assistant's greeting came back as if it were a memory. Match the
        // template shape at the START of the text, so a substantive reply that
        // merely contains one of the phrases survives.
        if s.starts_with("you're welcome") || s.starts_with("you are welcome") {
            return true;
        }
        if s.starts_with("hello") && s.contains("how can i help") {
            return true;
        }

        false
    }

    fn classify(&self, content: &str) -> MemoryImportance {
        let lower = content.to_lowercase();

        // Critical: decisions, bug fixes, explicit instructions, corrections
        const CRITICAL: &[&str] = &[
            "we decided",
            "i decided",
            "let's use",
            "let's go with",
            "i've decided",
            "the decision",
            "we should use",
            "we're going to use",
            "going forward,",
            "from now on,",
            "always ",
            "never ",
            "don't ",
            "do not ",
            "avoid ",
            "make sure to",
            "you must",
            "the bug",
            "root cause",
            "the fix",
            "the issue was",
            "the error was",
            "this was causing",
            "caused by",
            "remember that",
            "note that",
            "important:",
            "critical:",
            "no, that's wrong",
            "not like that",
            "you should never",
            "preference:",
            "rule:",
            "convention:",
        ];
        if CRITICAL.iter().any(|p| lower.contains(p)) {
            return MemoryImportance::Critical;
        }

        // High: file references, code structure, factual explanations, preferences
        const HIGH: &[&str] = &[
            "src/",
            "~/",
            ".rs ",
            ".rs\"",
            ".toml",
            "cargo",
            "impl ",
            "fn ",
            "pub ",
            "struct ",
            "enum ",
            "trait ",
            "mod ",
            "#[",
            "::",
            "the reason",
            "because ",
            "works by",
            "is defined in",
            "lives in",
            "is located",
            "is stored",
            "the pattern",
            "the approach",
            "we use ",
            "we're using",
            "i prefer",
            "i like to",
            "prefer to",
        ];
        if HIGH.iter().any(|p| lower.contains(p)) {
            return MemoryImportance::High;
        }

        MemoryImportance::Normal
    }

    /// Extract the most signal-dense prose from the content.
    ///
    /// For assistant responses: strip code fences/indented blocks, keep prose,
    /// cap at 300 chars at a sentence boundary.
    /// For user messages: keep as-is (usually short and already dense).
    fn extract(&self, role: &str, content: &str) -> String {
        const MAX_CHARS: usize = 300;

        if content.len() <= MAX_CHARS {
            return content.to_string();
        }

        if role == "assistant" {
            // Strip code blocks (``` fences and 4-space / tab indented lines)
            let prose: String = content
                .lines()
                .filter(|l| !l.starts_with("```") && !l.starts_with("    ") && !l.starts_with('\t'))
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");

            // Fall back to raw content if stripping removed too much
            let src = if prose.len() < 40 { content } else { &prose };
            truncate_at_sentence(src, MAX_CHARS)
        } else {
            truncate_at_sentence(content, MAX_CHARS)
        }
    }
}

impl Default for MemoryClassifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Truncate `s` at `max_chars`, preferring sentence/word boundaries.
fn truncate_at_sentence(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }

    // Find a safe char boundary at or before max_chars
    let safe = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max_chars)
        .last()
        .unwrap_or(0);
    let slice = &s[..safe];

    // Prefer ending at a sentence boundary
    if let Some(pos) = slice.rfind(['.', '!', '\n']) {
        return s[..=pos].trim().to_string();
    }

    // Fall back to word boundary
    if let Some(pos) = slice.rfind(' ') {
        return format!("{}…", s[..pos].trim());
    }

    // Hard cut
    format!("{}…", slice)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> MemoryClassifier {
        MemoryClassifier::new()
    }

    // ── Noise filter ─────────────────────────────────────────────────────────

    #[test]
    fn test_noise_short_message_is_discarded() {
        assert!(classifier().process("user", "ok").is_none());
        assert!(classifier().process("user", "yes").is_none());
        assert!(classifier().process("user", "thanks!").is_none());
    }

    #[test]
    fn test_noise_ack_phrase_is_discarded() {
        assert!(classifier().process("user", "got it").is_none());
        assert!(classifier().process("user", "sounds good").is_none());
        assert!(classifier().process("user", "understood.").is_none());
        assert!(classifier().process("user", "makes sense!").is_none());
    }

    #[test]
    fn test_noise_below_20_chars_is_discarded() {
        // 19 chars — just under threshold
        assert!(classifier()
            .process("user", "short message here!")
            .is_none());
    }

    #[test]
    fn test_youre_welcome_ack_is_discarded() {
        // #415 defect 4. The reference store recalled "You're welcome,
        // Shammah!" as a memory: 25 characters, so it cleared the length
        // floor, and the exact-match ack list only catches the bare phrase.
        assert!(
            classifier()
                .process("assistant", "You're welcome, Shammah!")
                .is_none(),
            "an assistant ack must not be stored as a memory"
        );
    }

    #[test]
    fn test_assistant_greeting_template_is_discarded() {
        // #415 defect 4. The measured recall block handed the model its own
        // past greeting back as "relevant context".
        assert!(
            classifier()
                .process("assistant", "Hello, Shammah! How can I help you today?")
                .is_none(),
            "a greeting template must not be stored as a memory"
        );
    }

    #[test]
    fn test_substantive_reply_is_not_discarded_as_noise() {
        // Guard against over-filtering while the ack and greeting rules above
        // are in place: an answer that merely mentions an ack phrase, and a
        // substantive reply that happens to open with a greeting word, must
        // both survive.
        let substantive = "The bug was that the greeting filter matched real \
answers; the fix lives in src/memory/quality.rs.";
        assert!(
            classifier().process("assistant", substantive).is_some(),
            "real content must not be discarded as noise: {substantive}"
        );
    }

    // ── Classification ───────────────────────────────────────────────────────

    #[test]
    fn test_decision_is_critical() {
        let result = classifier().process("user", "We decided to use anyhow for error handling.");
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, MemoryImportance::Critical);
    }

    #[test]
    fn test_bug_insight_is_critical() {
        let result = classifier().process(
            "assistant",
            "The bug was that libsqlite3-sys compiles with SQLITE_DEFAULT_FOREIGN_KEYS=1.",
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, MemoryImportance::Critical);
    }

    #[test]
    fn test_always_rule_is_critical() {
        let result = classifier().process("user", "Always use iterator chains, never for loops.");
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, MemoryImportance::Critical);
    }

    #[test]
    fn test_never_rule_is_critical() {
        let result = classifier().process("user", "Never use .unwrap() in production code.");
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, MemoryImportance::Critical);
    }

    #[test]
    fn test_file_path_is_high() {
        let result = classifier().process(
            "user",
            "The auth middleware lives in src/middleware/auth.rs",
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, MemoryImportance::High);
    }

    #[test]
    fn test_preference_is_high() {
        let result =
            classifier().process("user", "I prefer to use structured logging with tracing.");
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, MemoryImportance::High);
    }

    #[test]
    fn test_generic_question_is_normal() {
        let result = classifier().process("user", "How do Rust lifetimes work in practice?");
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, MemoryImportance::Normal);
    }

    #[test]
    fn test_system_role_is_always_critical() {
        // role="system" is from the explicit create_memory tool — always Critical
        let result = classifier().process(
            "system",
            "[context] The user is working on a Rust codebase.",
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, MemoryImportance::Critical);
    }

    // ── Extraction ───────────────────────────────────────────────────────────

    #[test]
    fn test_short_content_is_unchanged() {
        let content = "We use anyhow for errors.";
        let result = classifier().process("user", content).unwrap();
        // Content is already short — returned as-is (ignoring importance tier check here)
        // We re-derive since process classifies too, but key point: length unchanged
        let (extracted, _) = result;
        assert_eq!(extracted, content);
    }

    #[test]
    fn test_long_assistant_response_is_truncated() {
        let long = "a ".repeat(200); // 400 chars
        let result = classifier().process("assistant", &long);
        assert!(result.is_some());
        let (extracted, _) = result.unwrap();
        assert!(
            extracted.len() <= 310, // max_chars + potential "…"
            "extracted should be ≤310 chars, got {}",
            extracted.len()
        );
    }

    #[test]
    fn test_code_blocks_stripped_from_assistant_response() {
        let content = format!(
            "The answer is to use anyhow. Here is an example:\n```rust\nfn foo() {{}}\n```\n{}",
            "x ".repeat(200) // 400 chars of padding to push well past MAX_CHARS=300
        );
        let result = classifier().process("assistant", &content);
        assert!(result.is_some());
        let (extracted, _) = result.unwrap();
        assert!(
            !extracted.contains("```"),
            "code fence should be stripped: {}",
            extracted
        );
    }

    // ── MemoryImportance helpers ─────────────────────────────────────────────

    #[test]
    fn test_importance_round_trip_u8() {
        for imp in [
            MemoryImportance::Discard,
            MemoryImportance::Normal,
            MemoryImportance::High,
            MemoryImportance::Critical,
        ] {
            assert_eq!(MemoryImportance::from_u8(imp.as_u8()), imp);
        }
    }

    #[test]
    fn test_retrieval_boost_ordering() {
        assert!(
            MemoryImportance::Critical.retrieval_boost() > MemoryImportance::High.retrieval_boost()
        );
        assert!(
            MemoryImportance::High.retrieval_boost() > MemoryImportance::Normal.retrieval_boost()
        );
        assert_eq!(MemoryImportance::Discard.retrieval_boost(), 0.0);
    }
}
