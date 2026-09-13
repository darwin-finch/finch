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
