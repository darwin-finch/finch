// The `compress` role's contract with MemTree (issue: MemTree footer recap
// should be local-model parent summaries, not centroid snippets).
//
// A MemTree parent (internal) node's `text` is a label for the memories
// beneath it. Today it is a placeholder -- the first child's own wording,
// copied verbatim when the leaf promotes (`MemTree::promote_leaf`). This
// trait is what turns that placeholder into a real summary.
//
// `MemorySystem` never selects or downloads an implementation -- composition
// resolves one and injects it, exactly as `EmbeddingEngine` is injected. This
// keeps `finch-memory` free of ONNX/Candle/llama.cpp/HuggingFace dependencies
// (see `AGENTS.md`). No production implementation is wired yet: the bundled
// local-model loader this trait was designed alongside depended on the ONNX
// generator machinery (`OnnxLoader`/`LoadedOnnxModel`), which was removed
// when the model backend migrated to llama.cpp/GGUF. A composition-root
// implementation over that backend is follow-up work; until one exists,
// every caller passes `None` and `MemorySystem::refresh_pending_summaries`
// keeps parent labels at their last-known (or still-provisional) text.

use anyhow::Result;

/// The `compress` role: summarizes a MemTree parent's children into one
/// short label.
///
/// Implementations own whatever they need to answer this (a model session, a
/// cache, a lock) and must not block indefinitely -- a failure should return
/// `Err` promptly rather than hang the caller, since this runs off the
/// interactive read path (`MemorySystem::refresh_pending_summaries`), not the
/// insert/write path.
pub trait ParentSummarizer: Send + Sync {
    /// Summarize `children` -- the direct children's own text, in tree order
    /// -- into one short label suitable for a footer line.
    ///
    /// Returning an empty or whitespace-only string is treated by the caller
    /// the same as an error: the node's last-known label is kept.
    fn summarize_parent(&self, children: &[String]) -> Result<String>;
}

/// A summarizer whose output and call count a test can observe.
///
/// `pub(crate)` (not nested in `mod tests`) so `lib.rs`'s production-boundary
/// tests can prove the write path never calls it and the lazy refresh path
/// does.
#[cfg(test)]
pub(crate) struct RecordingSummarizer {
    pub(crate) calls: std::sync::atomic::AtomicUsize,
    output: String,
    /// When set, every call fails instead of returning `output`.
    fail: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
impl RecordingSummarizer {
    pub(crate) fn new(output: impl Into<String>) -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: output.into(),
            fail: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Make every subsequent call return `Err`, to exercise the
    /// generation-failed fallback path.
    pub(crate) fn set_failing(&self, failing: bool) {
        self.fail
            .store(failing, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
impl ParentSummarizer for RecordingSummarizer {
    fn summarize_parent(&self, _children: &[String]) -> Result<String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("RecordingSummarizer configured to fail");
        }
        Ok(self.output.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_summarizer_counts_calls_and_returns_configured_output() {
        let summarizer = RecordingSummarizer::new("topic: deploy keys");
        let result = summarizer
            .summarize_parent(&["a".to_string(), "b".to_string()])
            .expect("fake summarizer never fails");
        assert_eq!(result, "topic: deploy keys");
        assert_eq!(summarizer.calls(), 1);
    }

    #[test]
    fn test_recording_summarizer_fails_when_configured_to() {
        let summarizer = RecordingSummarizer::new("unused");
        summarizer.set_failing(true);
        let result = summarizer.summarize_parent(&["a".to_string()]);
        assert!(
            result.is_err(),
            "configured-to-fail summarizer must return Err"
        );
        assert_eq!(
            summarizer.calls(),
            1,
            "a failed call must still be counted, so the caller can be proven \
             to have attempted it"
        );
    }
}
