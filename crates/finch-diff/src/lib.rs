//! Bounded, terminal-safe structured diffs shared by Finch presenters.
//!
//! The application selects and supplies files, while this crate owns the
//! retained diff model, output bounds, terminal-control removal, and rendering.

mod diff;

pub use diff::{
    render_files, sanitize_multiline, sanitize_terminal, summarize_files, DiffColorMode, DiffHunk,
    DiffLine, DiffLineKind, FileDiff, MAX_DIFF_HUNKS, MAX_DIFF_INPUT_BYTES, MAX_DIFF_LINES,
    MAX_DIFF_LINE_CHARS, MAX_DIFF_PREVIEW_LINES, MAX_DIFF_STRUCTURAL_LINES,
};

/// `render_files` and `FileDiff::render` take `&ColorScheme` in their public
/// signatures; re-exporting it here lets a caller spell that argument's type
/// without adding its own direct `finch-theme` dependency. This matches the
/// existing convention at this exact architectural layer: `src/theme.rs`
/// (the root package's own compatibility facade for the extracted
/// `finch-theme` crate) and `finch-tui`'s crate root both re-export
/// `ColorScheme` from `finch-theme` rather than requiring it (#1033).
pub use finch_theme::ColorScheme;
