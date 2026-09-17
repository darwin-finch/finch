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

**Retained transcript disclosure** (`accordion.rs`, `view_model.rs`):
- The ViewModel projects a WorkUnit's `domain_view()` into `TranscriptNode` widget props with
  append-stable semantic row identity (`message id + semantic path`).
- Disclosure and focus are renderer state: `AccordionState` owns the open set keyed by row
  identity; a row's default is a projection-time prop, never state on domain data. Completing a
  run must not collapse a result that still has body text.
- Native scrollback always receives the fully expanded semantic projection;
  disclosure state only changes later reconstructed/live viewport projections.
- `F6`/`Shift+F6` moves semantic focus, Left/Right collapses or expands,
  Enter/Space toggles, and Escape returns focus to the draft. Left-click has the
  same toggle behavior when terminal mouse reporting is available.
- Disclosure state rides `row_expanded` metadata, never a visible `[expanded]`/`[collapsed]`
  token; neither color nor triangle shape is the sole carrier of state.
- Leaf lines render the ViewModel's label alone (the label already carries any status glyph);
  the disclosure never invents a `•` bullet (#821).
- Hit regions are the rects the layout pass claimed inside the transcript viewport, offset into
  terminal rows; the retained-transcript region above is recounted from physical-row geometry
  after every frame and resize. Never persist terminal coordinates as row identity.

**Claiming widget tree** (`widgets.rs`, `view_model.rs`): the live frame is a ViewModel
snapshot projected into a tree of standard widgets laid out by depth-first frame claiming. The
root column allocates chrome from the bottom (status, hr, input, hr, completions 0–N) and the
transcript viewport claims the leftover; an empty completions pane claims zero rows so the
composer and status never move (#232). Resize is a full layout pass — no widget keeps a cell
count from the previous frame. `Row` parents place children side by side, and `Side` tracks are
the width-conditional rails (#810).

**Dialog system** (`src/cli/tui/dialog.rs`):
- `Select` — Enter submits immediately; `o`/`O` or typing on Other row activates custom input
- `MultiSelect` — live prompts render the complete `↑/↓`, Space, Enter, and Esc keyboard hint; Space toggles, and Enter on the virtual Submit row emits `DialogResult::MultiSelected`
- `TextInput` — Enter submits
- `Confirm` — `y`/`n` or Enter/Esc
- Approval payload is a bounded, scrollable region. `dialog_lines` pins Yes/No/Cancel so a long write never moves the controls off-screen. Write approvals summarise path, size, and create-vs-overwrite; the full preview stays behind body scroll.

**Bounded tool-result controls** (`src/cli/tui/tool_viewport.rs`):
- Every `ToolOutput` transcript row is a reusable semantic control with a bounded
  child viewport (`DEFAULT_TOOL_OUTPUT_ROWS`, currently 4): each body line is
  truncated to the terminal width, so the bound is a hard row bound; one
  plain-text status row names the visible range, the total, and the
  scroll/expand affordances (`… lines 1–3 of 40 — ↑/↓ scroll · Enter expand`).
- Per-row scroll offsets live in `ToolViewportState`, keyed by the append-stable
  `view_model::RowId` — interleaved tool updates never reset them. Hit regions are
  rebuilt from physical-row geometry after every frame and resize, mirroring the
  accordion; a wheel whose X/Y lands inside a control scrolls that result only
  and keeps mouse tracking (native scrollback stays reachable for wheels off the
  control, per #441).
- Click on the control's cells, or Enter/Space with the row focused via F6,
  opens a focused expanded surface (title bar, scrolled body, plain-text
  footer). Up/Down/PageUp/PageDown/Home/End scroll it, Esc/q/Enter close it, and
  closing restores the captured child scroll offset, disclosure grouping, and
  focus. Ctrl+C and other unclaimed keys fall through to the input loop.
- Canonical native scrollback is never bounded: `commit_complete_messages`
  still writes the fully expanded projection exactly once, so the copyable
  record stays complete. The bound applies only to viewport projections.

Virtual row helpers:
- `dialog.submit_virtual_index()` — MultiSelect: `options.len() + (1 if allow_custom)`
- `dialog.cancel_virtual_index()` — Select: `options.len()`; MultiSelect: `submit + 1`

## Key files

- `src/cli/tui/mod.rs` — `TuiRenderer`, `flush_output_safe()`, `blit_visible_area()`
- `src/cli/tui/view_model.rs` — the blit-time `LiveViewModel`, the domain → widget projection, and the root claiming tree
- `src/cli/tui/widgets.rs` — claiming layout: rects, tracks, hitboxes, resize
- `src/cli/tui/shadow_buffer.rs` — `ShadowBuffer`, `diff_buffers()`, `visible_length()`
- `src/cli/tui/accordion.rs` — renderer-owned disclosure: open set, focus, hit regions
- `src/cli/tui/tool_viewport.rs` — bounded tool-result controls: child viewport state, wheel hit regions, expanded surface
- `src/cli/tui/scrollback.rs` — `ScrollbackBuffer`
- `src/cli/tui/dialog.rs` — Dialog state machine and approval control pin
- `src/cli/tui/input_widget.rs` — Input area (tui-textarea)
- `src/cli/tui/status_widget.rs` — Status bar
