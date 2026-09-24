//! Source languages accepted by the typed VM.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// One Co-Forth string/raw-string opener as recognized by the tokenizer.
///
/// The single canonical copy: `finch-coforth`'s real tokenizer builds its
/// `ForthLexicon` from this list, and any lighter-weight surface check
/// elsewhere (a syntax-vs-prose classifier, a CLI heuristic) reads the same
/// spellings instead of hand-maintaining its own copy that can silently
/// drift out of sync with what the tokenizer actually accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForthStringOpener {
    /// Source spelling including any `s` / `.` prefix, e.g. `s"""`, `."`.
    pub spelling: &'static str,
    /// Conventional Forth `s"` / `."` consume one delimiter whitespace.
    pub skip_one_ascii_ws: bool,
    /// Triple-quote raw literal; contents run until the next `"""`.
    pub raw: bool,
    /// `."text"` is sugar for a string token plus an implicit `say`.
    pub implicit_say: bool,
    /// Diagnostic code if this opener is not closed.
    pub unterminated_code: &'static str,
    /// Diagnostic message if this opener is not closed.
    pub unterminated_message: &'static str,
}

/// Co-Forth string openers, longest match first. See [`ForthStringOpener`].
pub const FORTH_STRING_OPENERS: &[ForthStringOpener] = &[
    ForthStringOpener {
        spelling: "s\"\"\"",
        skip_one_ascii_ws: false,
        raw: true,
        implicit_say: false,
        unterminated_code: "E-READ-004",
        unterminated_message: "unterminated Co-Forth raw string literal",
    },
    ForthStringOpener {
        spelling: "\"\"\"",
        skip_one_ascii_ws: false,
        raw: true,
        implicit_say: false,
        unterminated_code: "E-READ-004",
        unterminated_message: "unterminated Co-Forth raw string literal",
    },
    ForthStringOpener {
        spelling: ".\"",
        skip_one_ascii_ws: true,
        raw: false,
        implicit_say: true,
        unterminated_code: "E-READ-005",
        unterminated_message: "unterminated Co-Forth output string literal",
    },
    ForthStringOpener {
        spelling: "s\"",
        skip_one_ascii_ws: true,
        raw: false,
        implicit_say: false,
        unterminated_code: "E-READ-001",
        unterminated_message: "unterminated Co-Forth string literal",
    },
    ForthStringOpener {
        spelling: "\"",
        skip_one_ascii_ws: false,
        raw: false,
        implicit_say: false,
        unterminated_code: "E-READ-001",
        unterminated_message: "unterminated Co-Forth string literal",
    },
];

/// Language in which a stored program's canonical source is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramLanguage {
    Forth,
    Lisp,
}

impl ProgramLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forth => "forth",
            Self::Lisp => "lisp",
        }
    }

    /// Compact wire-format inference used only when the submission envelope
    /// omits `language`; the resolved value is recorded before execution.
    ///
    /// Detection only. A caller that strips leading Markdown noise before
    /// compiling (see `strip_markdown_backtick_noise` in
    /// `src/cli/repl_event/query_processor.rs`) must apply that same
    /// normalization before calling this, or the detected language and the
    /// compiled source can disagree: backtick is a real quasiquote reader
    /// macro in CoLisp, so classifying past a leading backtick here while
    /// leaving it in the compiled source silently turns a real definition
    /// into quoted, never-executed data instead.
    pub fn infer_source(source: &str) -> Self {
        if source.trim_start().starts_with('(') {
            Self::Lisp
        } else {
            Self::Forth
        }
    }

    /// Resolve the compact provider wire form before parsing. The first
    /// non-whitespace byte remains the intentionally cheap discriminator,
    /// while common non-protocol wrappers receive a useful error instead of
    /// being misreported as an unknown Co-Forth word.
    pub fn infer_wire_source(source: &str) -> Result<Self> {
        let trimmed = source.trim_start();
        if trimmed.is_empty() {
            bail!("E-WIRE-001: Finch wire response is empty; emit a Lisp or Co-Forth program")
        }
        if trimmed.starts_with("```") {
            bail!(
                "E-WIRE-002: Finch wire response must be raw Lisp/Co-Forth, not a Markdown code fence; \
                 emit s\"...\" say for user prose"
            )
        }
        Ok(Self::infer_source(trimmed))
    }
}

impl std::str::FromStr for ProgramLanguage {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "forth" => Ok(Self::Forth),
            "lisp" => Ok(Self::Lisp),
            other => bail!("unknown program language: {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_source_detects_plain_lisp_and_forth() {
        assert_eq!(
            ProgramLanguage::infer_source("(define (fib (n : int)) : int (say n))"),
            ProgramLanguage::Lisp
        );
        assert_eq!(
            ProgramLanguage::infer_source(": fib ( n -- n ) ;"),
            ProgramLanguage::Forth
        );
    }

    #[test]
    fn test_infer_source_treats_a_leading_backtick_as_forth_since_this_is_detection_only() {
        // infer_source is detection ONLY -- a real quasiquote-prefixed form
        // and a stray Markdown backtick are indistinguishable at this layer
        // by design. Normalizing a genuine Markdown artifact out of the
        // source belongs to the caller, before both detection and
        // compilation see it (strip_markdown_backtick_noise in
        // query_processor.rs), so detection and the compiled source can
        // never disagree about what the backtick means.
        assert_eq!(
            ProgramLanguage::infer_source("`(define (fib (n : int)) : int (say n))"),
            ProgramLanguage::Forth
        );
    }

    #[test]
    fn test_infer_wire_source_still_rejects_a_full_markdown_fence() {
        let error = ProgramLanguage::infer_wire_source("```lisp\n(say \"hi\")\n```")
            .expect_err("a real triple-backtick fence must still be rejected, not silently unwrapped");
        assert!(error.to_string().contains("E-WIRE-002"));
    }

    #[test]
    fn test_infer_wire_source_rejects_empty() {
        assert!(ProgramLanguage::infer_wire_source("   ").is_err());
    }
}
