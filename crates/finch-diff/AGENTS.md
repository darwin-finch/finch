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
terminal behavior; a later composition pass may decide how to inject it.

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
