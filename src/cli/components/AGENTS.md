# components capsule: component-owned presentation for typed messages

Supplements the root [`AGENTS.md`](../../AGENTS.md), which still applies in full.

**What this is.** The component layer of `docs/TUI_DESIGN.md`: the presentation half of one
message type. A component maintains a ViewModel (retained on the message, behind the
message's own lock — the ViewModel and its action payloads live beside the message in
`cli::messages` so the domain never depends upward), a renderer for the turn's single
representation per state, and subwidgets constructed from the outer ViewModel each frame that
choose to render or not. The say turn proves the model (#882, stages 1–2); stages 3–4 migrate
the remaining message types and add the DOM lowering.

**Dependency direction.** Dependencies point downward only: this module depends on
`cli::messages` (domain) and its own `vocab`, never on `cli::tui` (the engine), `crossterm`,
or the shadow buffer. The engine asks the `Message` trait for a component snapshot and hands
it here; it never matches on message type, and it carries component actions opaquely — there
is no central action enum.

**Interface:** the `pub(crate)` re-exports below are the whole surface; read them directly for
exact signatures.

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

One say turn renders as **one** representation per state (stage 2 of `docs/TUI_DESIGN.md`,
#882): Generating (no program yet) is one animated progress line (braille spinner frame from
sub-second elapsed + phase + time); Running is the program source inline, with any
already-arrived output bytes rendered beneath it — hiding arrived `say` bytes is the pre-#350
defect class, so they are never suppressed; Completed is the output prose inline plus the
`(ran Ns)` annotation, with the `ProgramSource`/`Output` subwidgets constructed from the outer
ViewModel each frame (`ProgramSource` replaces the prose only while `show_program`). There is
no chrome: the stage-1 card furniture (glyph, arrow, `[0]` hitbox) is deleted, and the
completed turn's toggle hit target is the output region itself — every completed content line
carries RowId path `[1]`, component-owned, with `row_expanded` carrying the disclosure state.
`card_lines` produces the transcript lines; the engine's claiming pass turns the output region
into the toggle hitboxes. Clicks and keyboard disclosure (F6/Enter) on a component-owned row
route through the `Message` trait to the ViewModel's lock — nothing here paints cells, keeps
renderer state, or observes anything: repaints stay pull-per-frame.
