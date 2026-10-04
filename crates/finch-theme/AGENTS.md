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

**Theme selects, `[colors]` overrides:** `active_theme` names the preset the UI renders
(`ColorTheme::from_name` → `to_scheme`). The saved `[colors]` table is an override layer: only keys
that differ from the preset are applied on load and written on save, and a table that merely
repeats a built-in preset is ignored (`ColorScheme::is_builtin_preset`). The overlay itself lives in
the root package (`src/config/colors.rs`: `resolve_colors`, `color_overrides`) —
`test_resolve_colors_ignores_a_saved_table_that_only_repeats_a_preset`,
`test_resolve_colors_applies_only_the_overridden_keys_over_the_theme` there, and
`test_active_theme_selects_the_scheme_and_colors_table_only_overrides` in `src/config/loader.rs`.

**Canvas roles are top-level:** `background`, `foreground`, `highlight_bg`, `highlight_fg`. The
renderer paints every row, transcript and live area alike, on `background` with `foreground` as the
default ink, and takes chrome and glyph colours from the scheme —
`test_canvas_and_chrome_follow_every_preset_scheme` in `crates/finch-tui/src/span_render.rs`.
`ColorScheme::ansi_color` is the scheme's rendering of the 16 ANSI colours, used at the paint seam
(`Canvas::paint_row`) and by the setup wizard (`theme_wizard_frame`) so text styled with a fixed
colour still follows the theme — `test_ansi_color_keeps_a_schemes_own_named_colours_and_maps_the_rest_to_roles`.
Presets that must hold contrast against their own canvas use explicit RGB, not ANSI names, because
a name resolves through the terminal profile's palette.

**Focused tests:**
`./scripts/test_brains.sh cargo test -p finch-theme --lib` for theme behavior, followed by
`scripts/factory/gates rust cli::tui::` and the supervised workspace suite when the public
surface or serialization changes. Use `scripts/seam_cost.py` for dependency evidence. Do not
create a generated `INTERFACE.md` catalog.
