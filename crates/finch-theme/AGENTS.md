# Finch theme agent contract

Supplements the root [AGENTS.md](../../AGENTS.md). The [README](README.md) traces configuration
and TUI callers; [`src/lib.rs`](src/lib.rs) is the flat facade.

**Owns:** theme selection vocabulary, serialized `ColorScheme`/`ColorSpec` roles and defaults,
semantic message bands, contrast-preserving preset schemes, and ratatui color conversion. It does
not own config files, UI state, terminal I/O, or rendering lifecycle.

**Dependencies:** `serde` for the saved color shape and `ratatui` for color/style values only.
No root Finch module, provider, Brain, runtime, or terminal session dependency may be added.
Keep child implementation private and add a flat export only for a demonstrated caller.

**Invariants:** preserve saved theme names, `ColorSpec` representation, role defaults, and
contrast behavior across a mechanical move. The same `ColorTheme::to_scheme` and
`ColorSpec::to_color` results must reach configuration and the renderer; no independent mapping
may grow in either caller.

**Focused tests:**
`./scripts/test_brains.sh cargo test -p finch-theme --lib` for theme behavior, followed by
`scripts/factory/gates rust cli::tui::` and the supervised workspace suite when the public
surface or serialization changes. Use `scripts/seam_cost.py` for dependency evidence. Do not
create a generated `INTERFACE.md` catalog.
