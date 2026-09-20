# TUI Architecture

Full documentation lives at `docs/TUI_ARCHITECTURE.md`.

## Quick reference

**Dual-layer rendering:**
- Canonical transcript commit — `commit_complete_messages()` prints complete
  messages above the live area, then spools a viewport of linefeeds so those
  rows land in native terminal scrollback (permanent) exactly once per
  message id. Native history is the copyable record, never the reader.
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

**Conversation ScrollView** (`scroll_view.rs`, #806): the transcript region —
the 805 root column's `Flex` `TRANSCRIPT` claim, the leftover frame under the
bottom chrome — is an in-app scroll view over the conversation. Renderer state
is one `offset_from_bottom` (physical rows hidden below the viewport; `0` is
follow mode). The window is derived at paint time: `scroll_window_split` cuts
the projected retained transcript at the offset, and the full-viewport repaint
and the retained hit-region rebuild share the same split, so paint and
hitboxes never diverge. Wheel ticks land on it wherever the pointer is inside
the claim and outside every nested tool-result control; PageUp/PageDown do the
same from the keyboard, independent of mouse tracking. While scrolled up, a
canonical commit anchors the window (`anchor_committed_rows`) so streaming
content cannot drag the reader. Mouse tracking is held by default
(`mouse_capture.rs`): the #441 release-on-first-wheel hybrid is retired —
native history is not the reader; drag-selection under capture remains open on
#221 and a capture opt-out on #244.

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

**Component-owned say turn (#882)** (`src/cli/components/`): a successful untitled `say`
turn renders through its component as **one representation per state** (stage 2 of
`docs/TUI_DESIGN.md`): Generating (no program yet) is one animated progress line; Running is
the program source inline, with any already-arrived output bytes beneath it (never hidden);
Completed is the output prose inline plus the `(ran Ns)` annotation — no Program source row,
no Brain run row, no result row, no card chrome. The stage-1 chrome (glyph + arrow + the `[0]`
hitbox) is deleted; the toggle hit target is the completed output region itself (semantic path
`[1]`), clicked or focused (F6/Enter) to swap prose↔program through `show_program` on the
retained ViewModel (`WorkUnitViewModel`, on the WorkUnit behind its own lock). The
`ProgramSource`/`Output` subwidgets are still built from the outer ViewModel each frame and a
hidden one claims zero rows. The legacy source-group row does not render beside the card: the
viewport pairing (`say_turn_consolidated_source_ids` in `tui/mod.rs`) suppresses the adjacent
completed Program-source unit whose bytes are the turn's program — byte identity holds by
construction in every producer path and a mismatch suppresses nothing — while the canonical
record keeps the raw program exactly once (`commit_complete_messages` is untouched). The
renderer's RowId-keyed open-set maps never hold component rows. On reconnect the replay
reconstructs the card from the journal itself (#970): `reconstruct_replayed_say_turn_cards`
rebuilds the ViewModel on the replayed run group from the Program/Result event pattern, so the
replayed transcript carries one representation per state like the live session; the live
rendering path is untouched. The widget vocabulary
(`Rect`/`Track`/`Axis`/`Widget`/`RenderedTranscriptLine`/`RowId`/line metrics) lives in
`cli::components::vocab` so components build subtrees without `crossterm` or the shadow
buffer; the engine re-exports it under its stable paths.

**Claiming widget tree** (`widgets.rs`, `view_model.rs`): the live frame is a ViewModel
snapshot projected into a tree of standard widgets laid out by depth-first frame claiming. The
root column allocates chrome from the bottom (status, hr, input, hr, completions 0–N) and the
transcript viewport claims the leftover; an empty completions pane claims zero rows so the
composer and status never move (#232). The session task list and the tracked child-agent rows
are furniture (#966): natural tracks between the transcript viewport and the completions pane —
zero rows when there is nothing to show, never scrollable content, and visible at every scroll
position (a finished session task claims no row; a finished tracked row still renders its `✓`).
While a dialog is open the column instead carries the dialog card as an inline child (#807) and
the composer/status yield; the furniture keeps its place above the separator so an open dialog
cannot hide it either. Resize is a full layout
pass — no widget keeps a cell count from the previous frame. `Row` parents place children side
by side, and `Side` tracks are the width-conditional rails (#810).

**The setup wizard rides the same tree** (`src/cli/tui/wizard_host.rs`, #812): the setup
wizard is no longer a second terminal app. `setup_wizard/render.rs` converts wizard
state into one `WizardView` snapshot (tab titles, section lines, help, one optional
overlay card); `plan_wizard_frame` projects it into the claiming `widgets` tree — a
column whose 3-row tab block and 1-row help claim natural extents and whose section
claims the leftover — and `WizardHost::paint` renders the frame into a `ShadowBuffer`,
diffs rows against the previous frame, and rewrites only the logical lines whose visible
rows changed. Wizard lines measure with `wizard_visible_length`/`wizard_physical_rows`
(#926): the emoji-aware widths a terminal actually renders, so planned rows equal
terminal rows and the row diff stays exact; box and tab top borders are glyph-only `─`
runs of exactly the frame width. Device-code/add-provider/cancel overlays are
`Widget::DialogCard` children
(the #807 contract: claimed rect, chrome pinned inside the card, help yields while the
card owns keys). The wizard keeps its own terminal lifecycle (raw mode, alternate
screen, mouse capture), so the #265 editor/PTY handoff is unchanged; the view props are
the speakable canonical form a GUI setup surface (#808) can consume. The general
z-compositor (#793) remains a follow-up.

**Dialog system** (`src/cli/tui/dialog.rs`):
- `Select` — Enter submits immediately; `o`/`O` or typing on Other row activates custom input
- `MultiSelect` — live prompts render the complete `↑/↓`, Space, Enter, and Esc keyboard hint; Space toggles, and Enter on the virtual Submit row emits `DialogResult::MultiSelected`
- `TextInput` — Enter submits
- `Confirm` — `y`/`n` or Enter/Esc
- Approval payload is a bounded, scrollable region. `dialog_lines` pins Yes/No/Cancel so a long write never moves the controls off-screen. Write approvals summarise path, size, and create-vs-overwrite; the full preview stays behind body scroll.

**Dialogs are conversation widgets** (#807): an open dialog is a `Widget::DialogCard`
claimed as an inline region of the root column (`DIALOG_CARD` key) below the still-projected
conversation, not a viewport-owning overlay. The card's lines are `dialog_lines`' pinned
output — Yes/No/Submit stay inside the card while the preview body scrolls inside it, the
#435 guarantee — re-rendered and padded to the claimed height so both claiming passes and
the erase estimator agree. After submit, `complete_dialog`/`settle_dialog` writes the
settled record (question, options with the picked marker, `Answer:` line) through the
standard canonical-commit pipeline; approval event semantics are unchanged.
`TabbedDialog` remains the alternate-screen wizard as a follow-up.

**Assistant prose markdown (#756)** (`markdown.rs`, `view_model.rs`): the one domain→widget
projection parses assistant prose once into a bounded block model — fenced code blocks,
emphasis, inline code, lists — and renders it to styled viewport body lines on
`TranscriptNode::body`, keeping the raw source lines on `TranscriptNode::raw_body`. The
viewport paints the rendered body; the canonical commit's fully-expanded projection paints the
raw body, so native scrollback stays the raw, copyable record with no markdown SGR. Fences stay
visible text (dimmed) and code bodies stay whitespace-exact in both targets, so raw/no-color
reading gets the same text semantics from characters, not color. No new dependency: the parser
covers exactly the prioritized constructs and degrades everything else (and malformed input) to
literal text. Program source/output, tool rows, and user input are structurally outside this
path; the dialog-option `markdown` preview keeps its own rendering.

**Bounded tool-result controls** (`src/cli/tui/tool_viewport.rs`):
- Every `ToolOutput` transcript row is a reusable semantic control with a bounded
  child viewport (`DEFAULT_TOOL_OUTPUT_ROWS`, currently 4): each body line is
  truncated to the terminal width, so the bound is a hard row bound; one
  plain-text status row names the visible range, the total, and the
  scroll/expand affordances (`… lines 1–3 of 40 — ↑/↓ scroll · Enter expand`).
- Per-row scroll offsets live in `ToolViewportState`, keyed by the append-stable
  `view_model::RowId` — interleaved tool updates never reset them. Hit regions are
  rebuilt from physical-row geometry after every frame and resize, mirroring the
  accordion; a wheel whose X/Y lands inside a control scrolls that result only.
  A wheel anywhere else in the transcript claim scrolls the conversation
  ScrollView (#806); a wheel over the bottom chrome is claimed by nobody.
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
- `src/cli/components/` — component-owned presentation: the widget vocabulary (`vocab.rs`)
  and the say-turn component (`say_turn.rs`) (#882)
- `src/cli/tui/view_model.rs` — the blit-time `LiveViewModel`, the domain → widget projection, and the root claiming tree
- `src/cli/tui/markdown.rs` — bounded assistant-prose markdown parse/render for the viewport (#756); raw source stays the canonical record
- `src/cli/tui/widgets.rs` — claiming layout: rects, tracks, hitboxes, resize
- `src/cli/tui/scroll_view.rs` — the conversation ScrollView: scroll offset, wheel claim, window split
- `src/cli/tui/shadow_buffer.rs` — `ShadowBuffer`, `diff_buffers()`, `visible_length()`
- `src/cli/tui/accordion.rs` — renderer-owned disclosure: open set, focus, hit regions
- `src/cli/tui/tool_viewport.rs` — bounded tool-result controls: child viewport state, wheel hit regions, expanded surface
- `src/cli/tui/scrollback.rs` — `ScrollbackBuffer`
- `src/cli/tui/dialog.rs` — Dialog state machine and approval control pin
- `src/cli/tui/wizard_host.rs` — the setup wizard's widget host: view snapshot, claiming plan, shadow-buffer row-diff blit (#812)
- `src/cli/tui/input_widget.rs` — Input area (tui-textarea)
- `src/cli/tui/status_widget.rs` — Status bar
