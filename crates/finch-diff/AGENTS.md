# Finch diff agent contract

Supplements the root [AGENTS.md](../../AGENTS.md). The [README](README.md) traces message and
TUI workflows; [`src/lib.rs`](src/lib.rs) is the flat facade.

**Owns:** bounded file-diff structure, summaries, terminal-control removal, and presentation
rendering. It does not own filesystem reads, approval decisions, message lifecycle, renderer
layout, or terminal I/O. Keep implementation in the private child module and add flat exports
only for demonstrated callers.

**Dependencies:** `finch-theme` supplies shared color meaning, `ratatui` supplies color values,
and `similar` computes textual changes. Do not import the root Finch package, config, messages,
tools, Brain, runtime, or TUI. Callers supply the text and paths. `DiffColorMode::production`
retains its existing `NO_COLOR`/`TERM` selection so this mechanical move does not change live
terminal behavior; a later composition pass may decide how to inject it. `render_files` and
`FileDiff::render` require `&ColorScheme`, so `src/lib.rs` re-exports it from `finch-theme`
(#1033) — matching `src/theme.rs` and `finch-tui`'s own re-exports of the same type — instead of
requiring every caller to add a direct `finch-theme` dependency just to spell the argument type.

**Public surface (#1033 audit, 2026-09-27):** a workspace-wide grep of every re-export/alias path
(not a definition-scoped LSP reference search, which produces false negatives across this crate's
boundary) found zero external callers, by any path, for `MAX_DIFF_FILES`, `MAX_DIFF_COMPUTE_LINES`,
`MAX_RENDER_CHARS`, `FileDiff::file_count_is_exact`, `DiffHunk::{old_start,old_count,new_start,
new_count,context}`, and `DiffLine::text`; all are narrowed to `pub(crate)` and remain used
internally. `DiffHunk` and `DiffLine` themselves stay `pub` regardless of caller evidence because
each is the element type of a `pub` field on a `pub` struct (`FileDiff::hunks: Vec<DiffHunk>`,
`DiffHunk::lines: Vec<DiffLine>`) — Rust's private-in-public rule forces this. Everything else in
the 18-export facade and the remaining 14 public methods (`is_complete` was already narrowed by
PR #1093; `file_count_is_exact` is this audit's own finding) and 7 model fields has a confirmed
real external caller (production or test) and stays `pub` unchanged.

**Invariants:** preserve the existing byte, line, file, hunk, preview, and render bounds. Escape
sequences and controls in untrusted paths or summaries must not reach terminal output. A
truncated diff must not claim completeness. Color/no-color selection may change styling bytes,
but must not change retained diff content or elision accounting. The moved implementation and
tests should stay behavior-identical except for imports needed at the crate boundary.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-diff --lib` for bounds,
sanitation, and rendering; `./scripts/test_brains.sh cargo test -p finch --lib cli::messages::`
and `./scripts/test_brains.sh cargo test -p finch --lib cli::tui::` for the two callers. Run
`cargo build --workspace` and the supervised workspace suite for a crate or facade move. Use
`scripts/seam_cost.py` for dependency evidence. Do not create a generated `INTERFACE.md`.
