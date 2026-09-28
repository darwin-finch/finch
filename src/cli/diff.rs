//! Compatibility path for the extracted bounded diff crate.
//!
//! New presentation code imports `finch_diff` directly; existing application
//! callers retain this path while their composition code is migrated.

pub use finch_diff::{
    render_files, sanitize_multiline, sanitize_terminal, summarize_files, ColorScheme,
    DiffColorMode, DiffHunk, DiffLine, DiffLineKind, FileDiff, MAX_DIFF_HUNKS,
    MAX_DIFF_INPUT_BYTES, MAX_DIFF_LINES, MAX_DIFF_LINE_CHARS, MAX_DIFF_PREVIEW_LINES,
    MAX_DIFF_STRUCTURAL_LINES,
};
