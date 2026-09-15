# TUI Architecture

Full documentation lives at `docs/TUI_ARCHITECTURE.md`.

## Quick reference

**Dual-layer rendering:**
- Canonical transcript commit — `commit_complete_messages()` prints complete
  messages above the live area, then spools a viewport of linefeeds so those
  rows land in native terminal scrollback (permanent) exactly once per
  message id.
- Live area — `erase_live_area()` + `draw_live_area()` erase and redraw the
  streaming WorkUnit, input, and status every frame via the shadow buffer.
  Redraws stay inside the terminal so they never enter native history.

**Critical invariant:** Each message is written to the terminal exactly once.
`commit_complete_messages()` skips ids already in `printed_ids` and marks a
message only after its staged bytes were written; `prepare_canonical_commit()`
clears the visible projection first, so a re-commit after a resize cannot spool
the row into native history twice. Test:
`canonical_commit_marks_only_after_success_and_follows_resize_clear` in
`src/cli/tui/mod.rs`.

**Retained transcript accordions** (`accordion.rs`):
- WorkUnits expose an append-stable semantic row tree (`message id + semantic path`).
- Native scrollback always receives the fully expanded semantic projection;
  disclosure state only changes later reconstructed/live viewport projections.
- `F6`/`Shift+F6` moves semantic focus, Left/Right collapses or expands,
  Enter/Space toggles, and Escape returns focus to the draft. Left-click has the
  same toggle behavior when terminal mouse reporting is available.
- Disclosure labels include `expanded`/`collapsed`; neither color nor triangle
  shape is the sole carrier of state.
- Hit regions are rebuilt from Unicode physical-row geometry after every frame
  and resize. Never persist terminal coordinates as row identity.

**Dialog system** (`src/cli/tui/dialog.rs`):
- `Select` — Enter submits immediately; `o`/`O` or typing on Other row activates custom input
- `MultiSelect` — Space toggles; Enter on virtual Submit row emits `DialogResult::Selected`
- `TextInput` — Enter submits
- `Confirm` — `y`/`n` or Enter/Esc
- Approval payload is a bounded, scrollable region. `dialog_lines` pins Yes/No/Cancel so a long write never moves the controls off-screen. Write approvals summarise path, size, and create-vs-overwrite; the full preview stays behind body scroll.

Virtual row helpers:
- `dialog.submit_virtual_index()` — MultiSelect: `options.len() + (1 if allow_custom)`
- `dialog.cancel_virtual_index()` — Select: `options.len()`; MultiSelect: `submit + 1`

## Key files

- `src/cli/tui/mod.rs` — `TuiRenderer`, `flush_output_safe()`, `commit_complete_messages()`, `erase_live_area()`/`draw_live_area()`
- `src/cli/tui/shadow_buffer.rs` — `ShadowBuffer`, `diff_buffers()`, `visible_length()`
- `src/cli/tui/accordion.rs` — retained semantic projection, focus, and hit regions
- `src/cli/tui/scrollback.rs` — `ScrollbackBuffer` (not yet wired into the main render path)
- `src/cli/tui/dialog.rs` — Dialog state machine and approval control pin
- `src/cli/tui/input_widget.rs` — Input area (tui-textarea)
- `src/cli/tui/status_widget.rs` — Status bar
