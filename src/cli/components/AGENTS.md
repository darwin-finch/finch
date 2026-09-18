# components capsule: component-owned presentation for typed messages

Supplements the root [`AGENTS.md`](../../AGENTS.md), which still applies in full.

**What this is.** The component layer of `docs/TUI_DESIGN.md`: the presentation half of one
message type. A component maintains a ViewModel (retained on the message, behind the
message's own lock — the ViewModel and its action payloads live beside the message in
`cli::messages` so the domain never depends upward), a chrome renderer, and subwidgets
constructed from the outer ViewModel each frame that choose to render or not. This module
proves the model with the say turn (#882, stage 1); stages 2–4 migrate the remaining
presentations and add the DOM lowering.

**Dependency direction.** Dependencies point downward only: this module depends on
`cli::messages` (domain) and its own `vocab`, never on `cli::tui` (the engine), `crossterm`,
or the shadow buffer. The engine asks the `Message` trait for a component snapshot and hands
it here; it never matches on message type, and it carries component actions opaquely — there
is no central action enum.

**Interface:** [`INTERFACE.md`](INTERFACE.md) is generated from the `pub(crate)` re-exports
below; regenerate with `python3 scripts/generate_interfaces.py --write` after changing them.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- cli::components::`.

## The widget vocabulary (`vocab.rs`)

`Rect`, `Track`, `Axis`, `Widget`, `Layout`, `RenderedTranscriptLine`, `RowId`, `NodeRole`,
and the pure line-metric functions (`visible_length`, `physical_rows`, …) plus the claiming
pass (`layout`). This is the module `docs/TUI_DESIGN.md` names under "Dependency direction":
both `cli::tui` (which re-exports it under its stable `widgets`/`shadow_buffer` paths) and
any future surface author depend on it, and it touches neither `crossterm` nor the shadow
buffer. Subwidget subtrees are data: a hidden subwidget contributes zero lines, so the
claiming pass records zero rows for it.

## The say-turn component (`say_turn.rs`)

One say turn's card: `chrome_line` (status glyph + elapsed + a disclosure arrow that renders
**only while the program source can be shown** — the dead-▼ defect is impossible by
construction) plus the `ProgramSource` and `Output` subwidgets, built from the outer
ViewModel each frame. `card_lines` produces the transcript lines; the engine's claiming pass
turns the chrome row into the disclosure hitbox. Clicks and keyboard disclosure on a
component-owned row route through the `Message` trait to the ViewModel's lock — nothing here
paints cells, keeps renderer state, or observes anything: repaints stay pull-per-frame.
