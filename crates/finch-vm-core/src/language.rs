//! Source languages accepted by the typed VM.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

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
    /// Ignores a small amount of leading backtick noise before the
    /// discriminator byte: a model that was told not to use Markdown still
    /// sometimes wraps a real `(...)` form in an inline-code backtick out of
    /// habit, and a bare `trim_start` alone left that one stray byte enough
    /// to misclassify real Lisp as Forth -- which then cascades into asking
    /// the model to "repair" already-correct Lisp as Forth instead of just
    /// dropping the backtick.
    pub fn infer_source(source: &str) -> Self {
        let trimmed = source
            .trim_start()
            .trim_start_matches('`')
            .trim_start();
        if trimmed.starts_with('(') {
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
    fn test_infer_source_sees_past_a_stray_leading_backtick() {
        // Reproduces a real rejected wire response: a model told not to use
        // Markdown still prefixed a real Lisp form with one inline-code
        // backtick out of habit. A bare `trim_start` alone let that single
        // byte misclassify genuine Lisp as Forth, cascading into a wire
        // repair request that told the model to rewrite correct Lisp as
        // (malformed) Forth instead of just dropping the backtick.
        let source = "`(define (fib (n : int)) : int\n  (if (<= n 1) n (+ (fib (- n 1)) (fib (- n 2)))))";
        assert_eq!(
            ProgramLanguage::infer_source(source),
            ProgramLanguage::Lisp,
            "a single leading backtick must not misclassify a real Lisp form as Forth: {source}"
        );
    }

    #[test]
    fn test_infer_source_handles_whitespace_between_backtick_and_paren() {
        assert_eq!(
            ProgramLanguage::infer_source("  ` (say \"hi\")"),
            ProgramLanguage::Lisp
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
