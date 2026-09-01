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
- Cleanup can access only a fully initialized per-generation record published after `tcgetattr`;
  the original termios, descriptor, and completed activation-stage bits are ordinary initialized
  fields, never a shared `MaybeUninit` snapshot.
- A replacement session may activate only after the prior generation restored termios and protocol
  modes, closed its descriptor, and returned to `INACTIVE`; stale generation cleanup is a no-op.
- Public `TuiRenderer` construction never changes process signal handlers. The Finch binary creates
  `BinaryTerminalSession` before renderer activation; SIGINT/SIGTERM/SIGHUP are armed and restored
  with each active terminal generation. One generation-independent process-lifetime trampoline
  publishes sticky per-signal bits to one process-lifetime monitor. A trampoline selected by the
  kernel before host-disposition restoration may enter after Drop or replacement re-arm without
  loading zero or misattributing a generation; pending delivery remains descriptor-free and is
  drained before signal ownership can be released.
- Emergency cleanup uses the generation's nonblocking, close-on-exec tty descriptor and never waits
  for the renderer/global mutex or ordinary stdout.
- Activation protocol writes and renderer output use the same per-generation admission gate, have
  absolute 100 ms deadlines and bounded write chunks, and revalidate after admission.
  The atomic lease distinguishes an admitted pre-effect token from an executing tty effect.
  `ACTIVE -> CLEANING` may CAS-revoke an application-stalled pre-effect token, whose stale guard can
  no longer publish or clear its replacement; it never overtakes an executing nonblocking write.
  Activation failures aggregate their stage and rollback errors, retain explicit `CLEANING` repair
  ownership on incomplete rollback, and never report a false restored state.
- Application waits and nonblocking write loops use absolute deadlines. Their bound assumes that a
  runnable thread executing Finch's short gate section is scheduled, and that supported tty/console
  kernel calls return. It is not a mathematical wall-clock claim against a frozen process, stopped
  thread, wedged device driver, or unsupported console implementation. When injected application
  progress is withheld, library cleanup returns `TimedOut` without resetting, admitting a
  replacement, re-enabling ordinary stdout, recording restoration, or retry-spinning. The same
  generation remains `CLEANING`; a later bounded attempt repairs it after an observable progress
  epoch. Binary signal termination retries at a bounded active cadence; the process-lifetime monitor
  uses a materially lower idle cadence. IPC Quit latches one decoded `ControlMessage` and retries
  bounded restoration on progress/recovery ticks without another message. The panic hook makes one
  bounded, latched restoration attempt and always returns to Rust's unwind policy rather than
  parking forever. These binary paths complete within their larger absolute deadline under this
  supported-progress precondition and never treat a failed restoration as successful.
- The actual non-Unix `TuiRenderer` owns a `PortableRendererSession` actor that couples the bounded
  exclusive lease, exact output generation, raw/protocol activation, bounded staged output, cleanup,
  and Drop. Every staged Write/Flush carries shared `Pending -> Executing/Cancelled -> Complete`
  state and an expiry. A caller reports timeout only if its CAS cancels `Pending`; if the actor wins,
  it waits for that single bounded effect. Thus no queued command can publish after its caller has
  reported timeout, even when the actor resumes while the session remains `ACTIVE`. Activation and
  rollback failures are aggregated. Failed, unknown, or backpressured cleanup relinquishes only its
  attempt owner, leaves the generation `CLEANING`, and rejects stale writers and replacements until
  retry repairs it. Windows CI compiles and exercises this exact production actor and actual
  callbacks when a console/ConPTY is present; redirected hosted runners emit an explicit gated
  acceptance marker and are not claimed as real-console conformance.

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
