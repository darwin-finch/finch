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
    ///
    /// The fence check scans the whole string, not just its start: a
    /// response can open with something that parses as a syntactically
    /// valid, self-contained-looking form (e.g. a stray inline `` `(fib
    /// 7)` `` mention left over from Markdown cleanup) and still carry a
    /// real ` ```lisp ` fence later on, holding the actual program. Handing
    /// that whole blob to the compiler executes only the leading fragment
    /// and never reaches the real definition inside the fence, which fails
    /// opaquely (e.g. `E-LINK-002: unknown Lisp function`) instead of
    /// surfacing the structured Markdown-fence correction.
    ///
    /// The scan skips content inside a Co-Forth raw string literal
    /// (`s"""..."""` / `"""..."""`) rather than treating any occurrence of
    /// `` ``` `` anywhere as disqualifying: `finch_programs::wrap_prose_as_say`
    /// deterministically re-embeds a rejected response's own literal text --
    /// including any fence marker that text happened to contain -- inside
    /// exactly this kind of raw string literal, and that wrapped `say`
    /// program is legitimate code, not a Markdown-wrapped response, so it
    /// must still compile. Without this exclusion, a rejected response whose
    /// text contains a fence would be rejected all over again on its own
    /// deterministic re-wrap, permanently failing instead of being echoed
    /// back as literal output. A real top-level fence -- one not enclosed in
    /// such a literal -- still catches the original "whole response is one
    /// fence" case this guarded, so no prior case regresses.
    pub fn infer_wire_source(source: &str) -> Result<Self> {
        let trimmed = source.trim_start();
        if trimmed.is_empty() {
            bail!("E-WIRE-001: Finch wire response is empty; emit a Lisp or Co-Forth program")
        }
        if contains_unenclosed_fence(trimmed) {
            bail!(
                "E-WIRE-002: Finch wire response must be raw Lisp/Co-Forth, not a Markdown code fence; \
                 emit s\"...\" say for user prose"
            )
        }
        Ok(Self::infer_source(trimmed))
    }
}

/// True if `source` contains a Markdown fence marker (`` ``` ``) that is not
/// enclosed within a Co-Forth raw string literal (`s"""..."""` /
/// `"""..."""`). See [`ProgramLanguage::infer_wire_source`] for why the
/// exclusion exists. Content inside an unterminated raw string literal is
/// not scanned either -- an unterminated literal is already a distinct
/// compile failure the parser itself reports, and any fence at its own
/// nesting depth (this scan does not resolve depth beyond one raw-string
/// span) is a narrower, deliberately unhandled edge case: it requires the
/// rejected text to contain both a fence marker and a literal `"""`
/// sequence, and the latter is already documented (`wrap_prose_as_say`) as
/// never observed in practice on its own.
fn contains_unenclosed_fence(source: &str) -> bool {
    let mut rest = source;
    loop {
        let next_fence = rest.find("```");
        let next_raw_open = rest.find("\"\"\"");
        match (next_fence, next_raw_open) {
            (None, _) => return false,
            (Some(_), None) => return true,
            (Some(fence_idx), Some(raw_idx)) => {
                if fence_idx < raw_idx {
                    return true;
                }
                let after_open = &rest[raw_idx + 3..];
                match after_open.find("\"\"\"") {
                    Some(close_idx) => rest = &after_open[close_idx + 3..],
                    // Unterminated raw string literal: nothing after it can
                    // be a genuine top-level fence either.
                    None => return false,
                }
            }
        }
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
        let error = ProgramLanguage::infer_wire_source("```lisp\n(say \"hi\")\n```").expect_err(
            "a real triple-backtick fence must still be rejected, not silently unwrapped",
        );
        assert!(error.to_string().contains("E-WIRE-002"));
    }

    #[test]
    fn test_infer_wire_source_rejects_a_fence_occurring_later_in_the_string() {
        // The untested shape that produced `error[E-LINK-002]: unknown Lisp
        // function 'fib'`: the response opens with a syntactically valid,
        // self-contained-looking form (a leftover inline `(fib 7)` mention)
        // but carries the real fenced program later on. Before this fix,
        // `starts_with("```")` only looked at the very front of the string,
        // so this whole blob passed through as plain Lisp and got handed to
        // the compiler -- which only ever saw the leading fragment and never
        // reached the real `defun`-equivalent definition inside the fence.
        let source = "(fib 7)`\n\nHere's a complete solution:\n\n```lisp\n\
                       (define (fib (n : int)) : int (if (<= n 1) n (+ (fib (- n 1)) (fib (- n 2)))))\n\
                       (say (int-to-string (fib 7)))\n```\n\nLet me know if you have more questions!";
        let error = ProgramLanguage::infer_wire_source(source).expect_err(
            "a fence occurring anywhere in the response must be rejected with the structured \
             E-WIRE-002 correction, not silently executed as a truncated Lisp fragment",
        );
        assert!(
            error.to_string().contains("E-WIRE-002"),
            "expected the Markdown-fence wire error, got: {error}"
        );
    }

    #[test]
    fn test_infer_wire_source_accepts_a_fence_marker_enclosed_in_a_raw_string_literal() {
        // Second-order regression this same fix must not introduce: when a
        // rejected response is deterministically re-wrapped as literal text
        // (`finch_programs::wrap_prose_as_say`, invoked because the source
        // never started with `(` or `:`), the wrap re-embeds the exact
        // rejected text -- including any fence marker it happened to
        // contain -- inside a `s"""..."""` raw string literal. That wrapped
        // program is legitimate `say` code, not a Markdown-wrapped response,
        // and re-validating it with the same fence check must not reject it
        // all over again; doing so would turn a safe, echo-the-text-back
        // fallback into a permanent failure instead.
        let rejected_text = "(fib 7)`\n\nHere's a complete solution:\n\n```lisp\n\
                              (define (fib (n : int)) : int (if (<= n 1) n (+ (fib (- n 1)) (fib (- n 2)))))\n\
                              (say (int-to-string (fib 7)))\n```\n\nLet me know if you have more questions!";
        let wrapped = format!("s\"\"\"{rejected_text}\"\"\" say");
        let language = ProgramLanguage::infer_wire_source(&wrapped).unwrap_or_else(|error| {
            panic!(
                "the wrap-as-say fallback must always compile as valid Forth, since it is the \
                 deterministic safety net for rejected wire responses; got error: {error}, \
                 wrapped source: {wrapped:?}"
            )
        });
        assert_eq!(
            language,
            ProgramLanguage::Forth,
            "the raw `s\"\"\"...\"\"\" say` wrap form is always Forth"
        );
    }

    #[test]
    fn test_infer_wire_source_still_accepts_plain_lisp_and_forth_with_no_fence() {
        // No prior case regresses: ordinary wire responses that never
        // mention a fence anywhere still resolve normally.
        assert_eq!(
            ProgramLanguage::infer_wire_source("(say \"hi\")").unwrap(),
            ProgramLanguage::Lisp
        );
        assert_eq!(
            ProgramLanguage::infer_wire_source("s\"hi\" say").unwrap(),
            ProgramLanguage::Forth
        );
    }

    #[test]
    fn test_infer_wire_source_rejects_empty() {
        assert!(ProgramLanguage::infer_wire_source("   ").is_err());
    }
}
