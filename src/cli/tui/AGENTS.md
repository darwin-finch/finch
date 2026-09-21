# tui capsule: terminal renderer and widgets

Supplements the root [`AGENTS.md`](../../../CLAUDE.md), which still applies in full.

**Owns** `src/cli/tui/`: the interactive terminal renderer (`TuiRenderer`), dialogs, the live
area, scrollback, the ViewModel projection, the claiming widget tree, the conversation
ScrollView, disclosure (accordion), and activity rows.
This is not a published crate. The test is whether production code here can draw without naming
Finch's poset, tool, or runtime vocabularies.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every item the facade re-exports. Child
modules stay private except `activity`, which callers already name. Add public surface by
re-exporting it from `mod.rs`, then regenerate with `python3 scripts/generate_interfaces.py --write`.
`view_model` is `pub(crate)`: projection-feeding consumers and their tests project messages
through it, so it is reachable crate-wide but is not published facade surface.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- cli::tui::`.

## The blit pipeline: ViewModel → widget tree → claiming → paint

Every blit converts domain state into one owned ViewModel snapshot, then lays it out:

1. **`view_model::LiveViewModel`** is the blit-time snapshot — composer draft + cursor,
   completion rows (empty unless the draft is a slash command or `@` mention), status lines,
   live transcript lines, dialogs. `live_frame_sources` gathers the owned state;
   `plan_live_frame` plans the frame from it and is a pure function (assertable without a
   terminal).
2. **`view_model::project_message`** is the one domain → widget conversion: a WorkUnit's
   `domain_view()` (plain domain data — labels, statuses, bodies; see
   `src/cli/messages/AGENTS.md`) becomes `TranscriptNode` widget props. Widgets never query
   `WorkUnit`, the command registry, or each other to decide visibility; a widget with nothing
   to show claims zero rows and stays in the tree.
3. **`widgets::layout`** is depth-first **frame claiming**: a parent offers a box, children
   claim sub-rectangles (`Track::{Natural, Flex, Max, Side}`), the pass records claimed rects
   and disclosure hitboxes. Resize is another pass — no widget keeps a cell count from the
   previous frame. The TUI root is one column that allocates chrome **from the bottom**
   (status, hr, input, hr, completions 0–N) with the transcript viewport claiming the
   leftover; a `Row`/grid parent places children side by side, and `Side` tracks are the
   width-conditional rails (#805, #809, #810). The session task list and the tracked
   child-agent rows are **furniture** (#966): natural tracks between the transcript viewport
   and the completions pane that claim zero rows when there is nothing to show, never ride
   scrollable viewport content, and stay on the frame at every scroll position (the frame
   yields its oldest furniture rows first when it cannot afford them all, so the composer
   and status never move). The status-strip chips ride the `STATUS` natural claim the same
   way — they are projected from the status bar (`effective_status`), never from scroll
   content.
4. Painting stays line-based on the claimed rects; native `canonical_commit` remains the
   separate once-per-id pipeline.

## The widget vocabulary lives in `finch-ui-model` (#877/#882)

`Rect`, `Track`, `Axis`, `Widget`, `Layout`, `RenderedTranscriptLine`, `RowId`, `NodeRole`,
and the pure line-metric functions live in `finch-ui-model`, reached through the root
`crate::ui_model` compatibility facade (stage-1
prerequisite of `docs/TUI_DESIGN.md`) so a component can build and claim a subtree without
touching `crossterm` or the shadow buffer. The engine keeps its stable `widgets` /
`shadow_buffer` paths as re-exports; new surface authors depend on the vocabulary directly.

## Component-owned say turns (#882, stages 1–2)

A successful untitled `say` turn renders through its **component** as **one representation per
state** (stage 2 of `docs/TUI_DESIGN.md`): `TuiRenderer::projected_message_lines` asks the
`Message` trait for `say_turn_view()` and hands the snapshot to `cli::components::card_lines`,
which renders Generating as one animated line, Running as the program source inline (arrived
output bytes beneath it, never hidden), and Completed as the output prose plus `(ran Ns)` —
no Program source row, no Brain run row, no result row, no card chrome. The stage-1 chrome
(glyph + arrow + the `[0]` hitbox) is deleted; the toggle hit target is the completed output
region (semantic path `[1]`, clicked or driven by F6/Enter), routing the opaque action to the
message's `handle_transcript_action`, which toggles `show_program` under the message's lock;
the next frame re-renders from the mutated ViewModel. The legacy source-group row does not
render beside the card: `TuiRenderer::projected_lines` pairs each say-VM unit with the
adjacent completed Program-source unit whose response text is byte-identical to the turn's
program (`say_turn_consolidated_source_ids`) and suppresses that row from the viewport only —
byte identity holds by construction in every producer path and a mismatch suppresses nothing,
so the rule fails toward rendering more, never less. The canonical record is untouched
(`commit_complete_messages` has no neighbour context): the raw program and the say bytes still
spool exactly once, and the pinned canonical-commit invariants pass as-is. The renderer's
RowId-keyed open-set maps never hold component rows: they register in
`AccordionState::component_regions` for routing only. Unmigrated rows keep the maps and the
projection path.

## Assistant prose markdown renders in the viewport (#756)

`markdown.rs` (private) is a deliberately bounded inline-subset parser — fenced code blocks,
bold/italic emphasis, inline code, ordered/unordered lists — chosen over a markdown crate by
the #756 contract: the acceptance surface is those constructs, everything else must degrade to
literal text anyway, and the render target is a custom ANSI line model regardless. It is
assistant prose only (`WorkUnitPresentation::Assistant`); program source/output, tool rows,
activity rows, and user queries never reach it, and the dialog-option `markdown` preview keeps
its own path.

One representation, two render targets: `project_work_unit` parses the assistant head body once
and puts the rendered lines in `TranscriptNode::body` (the viewport projection) and the raw
source lines in `TranscriptNode::raw_body` (`None` when the text needed no rendering).
`AccordionState::render_row` paints `body`; the canonical commit's fully-expanded rendering
paints `raw_body`, so native scrollback keeps the RAW source byte-identical to the
pre-markdown behavior — the copyable record is never the rendered form, and no markdown SGR
enters it. Code-block bodies stay whitespace-exact in both targets; the fence lines remain
visible text (dimmed in the viewport), so no-color reading relies on characters, not color.
Malformed input and outside-the-subset constructs pass through literally without panicking.

## The conversation ScrollView owns reading (#806)

The transcript region — the root column's `Flex` `TRANSCRIPT` claim, the leftover frame
under the bottom chrome — is an in-app scroll view (`scroll_view.rs`). Renderer state is one
offset from the bottom of the retained transcript (0 = follow mode); the window is derived
at paint time by `scroll_window_split`, shared by the full-viewport repaint and the
retained hit-region rebuild. Wheel ticks land on the ScrollView inside the claim, on a
nested tool-result control inside its rect, and on nothing over the bottom chrome; the
claim is the wheel hitbox, stored from `frame.rects` by the same rebuild that rebuilds the
accordion regions. PageUp/PageDown scroll it from the keyboard, independent of mouse
tracking. Mouse tracking is held by default (`mouse_capture.rs`) — the #441
release-on-first-wheel hybrid is retired, native history stays the copyable record via
`canonical_commit`, and while scrolled up a commit anchors the window instead of dragging
the reader.

## Dialogs are conversation widgets (#807)

An open dialog is an inline card claimed by the widget tree, not a global overlay. When
`vm.dialog` is set, `project_root` places a `Widget::DialogCard` child (`Track::Natural`,
marked `DIALOG_CARD`) between the transcript viewport and the session separator; the
conversation above stays projected and the ScrollView keeps its claim, while the composer
and status yield for the duration (the dialog owns the keys, exactly as the old overlay
did). The card's lines are the pinned output of `TuiRenderer::dialog_lines` — the
`pin_dialog_controls` discipline that keeps Yes/No/Submit inside the card while the
preview body scrolls inside it (#435) — re-rendered to, and padded to exactly, the
height the sizing pass claimed, so both claiming passes see the same box and the erase
estimator (`live_geometry`) plans the identical frame ("one planner, two consumers").
Wheels and clicks over the conversation stay gated while a dialog owns focus. Overlay
placement (a z-layer above the conversation, #713/#793) is the same widget under a
different parent; it is not a second renderer and must blit through the shadow buffer.

After submit the renderer freezes the settled card into the conversation:
`complete_dialog` (async input) / `settle_dialog` (blocking `show_dialog`) writes a
speakable, sanitised record — the question, every option with its radio/checkbox state at
submit time, and an explicit `Answer:` line — into the OutputManager, so the standard
exactly-once canonical-commit pipeline carries it into native scrollback. Approval
event semantics (`ToolApprovalNeeded` routing, `pending_dialog_result` consumers) are
untouched. `TabbedDialog` (2+ question cards) is still the ratatui alternate-screen
wizard and is a follow-up.

## The setup wizard rides the same tree (#812)

`wizard_host.rs` hosts the setup wizard on the claiming widget tree and the shadow
buffer, so there is no second terminal app: `setup_wizard` converts its state into one
owned `WizardView` snapshot (tab titles, section lines, help, one optional overlay card)
and `plan_wizard_frame` projects it into the standard `widgets` tree — a column whose
3-row tab block and 1-row help claim natural extents and whose section claims the
leftover, exactly like the conversation root allocates chrome. Device-code, add-provider,
and cancel-confirmation overlays are `Widget::DialogCard` children (#807 contract:
claimed rect, title and controls pinned inside the card, the help line yields while the
card owns keys). `WizardHost::paint` renders the frame into a `ShadowBuffer`, diffs rows
against the previous frame, and rewrites only the logical lines whose visible rows
changed — the buffer is the authority on what a reader sees. The wizard keeps its own
terminal lifecycle (raw mode, alternate screen, mouse capture) so the #265 editor/PTY
handoff is untouched, and its view props are the speakable canonical form a GUI setup
surface (#808) can consume. `WizardColor` is the view's own colour vocabulary, not the
renderer's. The general z-compositor (#793) stays a follow-up; this is the inline
dialog-card mechanism only.

**Wizard lines are measured by what a terminal renders (#926).** The row-diff blit is
only exact when the planner's row count equals the terminal's, so wizard line builders,
the frame planner, and the view projection all measure with `wizard_visible_length` /
`wizard_physical_rows` — the vocabulary's widths plus the emoji-presentation codepoints
terminals paint double-width (🔧 ❌ ✅). Box and tab top borders are glyph-only `─` runs
of exactly the frame width (`wizard_title_border`); a border carrying its gap count as
digits, or a padded row one column over the frame, desyncs the blit and mangles every
frame after it. The shared `visible_length`/`physical_rows` vocabulary keeps the narrower
measure for the conversation live area; extending it there is not this module's call.

## View types the renderer owns

The same inversion the todo list and child-agent rows already use (`activity.rs`): the renderer
draws a view, and the caller converts.

| View | What the renderer needs | Converted from |
|------|-------------------------|----------------|
| `view_model::TranscriptNode` | label, body, children, role, default disclosure | WorkUnit `domain_view()`, in `view_model::project_message` |
| [`activity::ActivityRow`](activity.rs) | indented status text | todos / agent tasks, in `cli::repl_event::activity_view` |
| `Dialog::tool_approval(name, summary)` | a name and a summary line | Finch `ToolUse`, in `cli::repl_event::tool_display::tool_approval_dialog` |
| [`cell_format::workbook_cell_to_string`](cell_format.rs) | one cell as text | calamine `Data`, inside `spreadsheet_preview_rows` |

When `active_dialog` first occupies the live surface, `draw_live_area` writes one terminal bell
(`\x07`). Redraws of the same pending card stay silent. OS notifications, duration-threshold
run-complete toasts, and a config off-switch remain follow-up on #752 (notify on attention-needed).

## Disclosure and focus are renderer state

`AccordionState` (in `accordion.rs`) is the single owner of open/closed, keyed by the stable
`RowId` (message id + append-only semantic path). A row's default is a projection-time prop
(`TranscriptNode::default_open`, derived from domain status); the user's toggle overrides it and
survives re-projection, streaming appends, terminal reflow, and reconnects. Completing a run
cannot collapse a result. Leaf lines render exactly the ViewModel's label — which carries the
status glyph — and never invent a `•` bullet or sniff glyph characters (#821).

The renderer no longer accepts or stores Finch's `Poset`. The old `set_poset` injection was
write-only and its Finch-to-`GraphView` adapter had no production caller, so #996 removed both.
The remaining graph-to-Forth projection and its public view vocabulary then had only self-tests,
so #1002 deleted them. `draw_poset_overlay` continues to paint the user-defined `check` word from
`corner` without accepting a graph or poset.

## Deliberate remaining production references

Acceptance for #623 (terminal framework standing alone) is: no production reference to any other
module of this crate, **or** the remaining ones are written down here with the reason.

**Finch modules this directory still names in production, and why they stay:**

- **`crate::workbook::{bounded_worksheet_range, MAX_WORKBOOK_CELLS}` in `spreadsheet_preview_rows`**
  — the file viewer still lives in this renderer. It opens a workbook and maps cells through
  tui-owned `cell_format`. Pulling the viewer out is the cheaper cut if the framework is ever
  extracted; until then the bound stays next to the preview that needs it.
- **`crate::theme::ColorScheme`** — leaf colour scheme, re-exported so callers write
  `crate::cli::tui::ColorScheme`. Theme is not Finch domain vocabulary.
- **Sibling CLI types** (`cli::messages`, `cli::diff`, `cli::llm_dialogs`,
  `cli::command_autocomplete`, `cli::suggestions`, `StatusBar`, `AskUserQuestion*`) — the renderer
  consumes `cli::messages` domain snapshots (`WorkUnitView`/`WorkUnitHead`) and projects them
  itself; `cli::diff` renders diff bodies at projection time.
- **`crate::context::mention`** — picker rows and pending snapshots. Filesystem policy,
  ignore rules, budgets, and digest identity live in `context::mention`; the renderer only
  draws speakable rows and inserts the visible token.
- **`crate::ABOUT`** — startup header copy.
- **`crate::is_editor_active` and `crate::finch_ipc_capnp` in `async_input.rs`** — the input task
  must not steal keys while `$EDITOR` is in the foreground, and it talks to the local control
  socket. That is process glue, not a widget.

**Not production, documented so a grep is not a surprise:**

- **`crate::tools::ToolUse` in `dialog.rs` tests** — three fixtures still build a `ToolUse` so they
  can drive `cli::repl_event::tool_display::tool_approval_dialog`, which is the production assembler
  (and lives outside this capsule). Dialog production code takes a name and a summary.
`test_tui_production_does_not_name_finch_tools_or_runtime` fails if production source grows a
`crate::tools` or `crate::runtime` name, and
`test_tui_production_does_not_name_finch_poset` keeps the deleted write-only Poset edge from
returning. `test_scanner_would_fail_if_runtime_returned_to_spreadsheet_preview_rows` and
`test_scanner_would_fail_if_tools_returned_to_tool_approval` fail if the scanner can no longer
see those production functions (the original leak sites).
