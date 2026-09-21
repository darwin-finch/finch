//! Bounded, terminal-safe structured diffs shared by Finch presenters.
//!
//! The application selects and supplies files, while this crate owns the
//! retained diff model, output bounds, terminal-control removal, and rendering.

mod diff;

pub use diff::{
    render_files, sanitize_multiline, sanitize_terminal, summarize_files, DiffColorMode, DiffHunk,
    DiffLine, DiffLineKind, FileDiff, MAX_DIFF_COMPUTE_LINES, MAX_DIFF_FILES, MAX_DIFF_HUNKS,
    MAX_DIFF_INPUT_BYTES, MAX_DIFF_LINES, MAX_DIFF_LINE_CHARS, MAX_DIFF_PREVIEW_LINES,
    MAX_DIFF_STRUCTURAL_LINES, MAX_RENDER_CHARS,
};
