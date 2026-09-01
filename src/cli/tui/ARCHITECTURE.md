# TUI Architecture

Full documentation lives at `docs/TUI_ARCHITECTURE.md`.

## Quick reference

**Dual-layer rendering:**
- `insert_before()` — new messages only; writes to terminal scrollback permanently
- `blit_visible_area()` — diff-based updates to in-viewport area only

**Critical invariant:** Each message is passed to `insert_before()` exactly once.
Check: `scrollback.get_message(msg_id).is_none()` before calling.

**Terminal-session lifecycle:**
- A process-global generation token is `ACTIVATING`, `ACTIVE`, or `CLEANING` for the complete
  interval in which its tty snapshot, protocol modes, or restore descriptor can still be used.
- A replacement session may activate only after the prior generation restored termios and protocol
  modes, closed its descriptor, and returned to `INACTIVE`; stale generation cleanup is a no-op.
- Public `TuiRenderer` construction never changes process signal handlers. The Finch binary creates
  `BinaryTerminalSession` before renderer activation; SIGINT/SIGTERM/SIGHUP are armed and restored
  with each active terminal generation. Its handler captures one atomic owner+generation token and
  publishes into per-signal atomic pending slots; it owns no pipe/socket descriptor that can leak
  across `exec` or be reused underneath a stale handler.
- Emergency cleanup uses the generation's nonblocking, close-on-exec tty descriptor and never waits
  for the renderer/global mutex or ordinary stdout.
- Renderer output is admitted against the active generation, serialized only around a nonblocking
  tty write, and revalidated after admission. `ACTIVE -> CLEANING` revokes parked writers before
  reset; a writer-gate timeout leaves the generation fail-closed for bounded takeover and never
  records restoration prematurely.
- The 100 ms writer-quiescence bound assumes normal scheduler progress for a thread executing the
  short `O_NONBLOCK` write section. If a thread is artificially suspended while holding that gate,
  cleanup returns a timeout without resetting, admitting a replacement, re-enabling ordinary
  stdout, or recording restoration; after the writer resumes and observes revocation, a later
  bounded cleanup owner repairs the same generation.
- The actual non-Unix `TuiRenderer` owns the same bounded exclusive session lease across activation,
  suspend/resume, emergency cleanup, shutdown, and Drop. Windows CI compiles and tests that exact
  lifecycle source together with the portable protocol source.

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

Virtual row helpers:
- `dialog.submit_virtual_index()` — MultiSelect: `options.len() + (1 if allow_custom)`
- `dialog.cancel_virtual_index()` — Select: `options.len()`; MultiSelect: `submit + 1`

## Key files

- `src/cli/tui/mod.rs` — `TuiRenderer`, `flush_output_safe()`, `blit_visible_area()`
- `src/cli/tui/shadow_buffer.rs` — `ShadowBuffer`, `diff_buffers()`, `visible_length()`
- `src/cli/tui/accordion.rs` — retained semantic projection, focus, and hit regions
- `src/cli/tui/scrollback.rs` — `ScrollbackBuffer`
- `src/cli/tui/dialog.rs` — Dialog state machine (7 regression tests)
- `src/cli/tui/input_widget.rs` — Input area (tui-textarea)
- `src/cli/tui/status_widget.rs` — Status bar
