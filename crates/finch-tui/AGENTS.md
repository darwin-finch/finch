# finch-tui capsule: terminal renderer and widgets

Supplements the root [`AGENTS.md`](../../AGENTS.md), which still applies in full.

**Owns** `crates/finch-tui/`: the interactive terminal renderer (`TuiRenderer`), dialogs, the live
area, scrollback, the ViewModel projection, the claiming widget tree, the conversation
ScrollView, disclosure (accordion), and activity rows.
This is an unpublished workspace crate. The test is whether production code here can draw without naming
Finch's poset, tool, or runtime vocabularies.

**Boundary:** the [README](README.md) explains the two caller workflows and ownership.
[`src/lib.rs`](src/lib.rs) is the facade; child modules stay private. The presentation types that
application callers need are re-exported flat, including activity rows and updates. Add public
surface only when a real caller needs it. Do not recreate a generated symbol catalog.
`view_model` is private to this module. Application tests project messages through the
lower message/UI-model contracts instead of reaching into renderer implementation.
`TuiStatusPort` is the stateful status seam: CLI `StatusBar` implements it, and the renderer
reads printable status/session snapshots and reports child-activity/operation updates through
that port. Keep status ordering and status-line policy in the application; do not import
`StatusBar` into production TUI code or add a port around pure line formatting.
`TuiOutputPort` is the stateful conversation-output seam: CLI `OutputManager` implements it,
retains message identity, controls stdout, and accepts settled dialog records. Production TUI
code must not import `OutputManager`; blit and canonical commit read its message snapshots.
`DiagnosticConsolePort` is the diagnostic-output seam: the application reads and sanitises its
frontend/daemon logs and supplies a bounded snapshot; the renderer owns only the Ctrl+` reader in
the transcript viewport, its visible-range indicator, and scrolling. Focused tool-result readers
use the same viewport-above-chrome path. Filesystem paths and log retention policy stay outside
this crate.
The renderer owns its composer draft and failed-frame recovery state. Application callers use
`restore_input_draft`, `record_render_failure`, and `take_render_failure_for_retry`; they must
not mutate the textarea, refresh flag, or render-error slot directly. A failed process
replacement uses `resume_after_emergency_restore` to reacquire terminal modes.
The event loop supplies package-version/tagline text for the startup header and a function
that reports whether an external editor owns the terminal. The input task must consult that
query before polling and before rendering; quit control messages use `finch-ipc` directly.
`async_input` also owns the keyboard-binding catalog (`KEYBOARD_SHORTCUTS`, re-exported flat):
its `ComposerShortcut` entries are the dispatch guards of the composer shortcut handler, and
the CLI `/help` renderer (`cli::commands::format_help`) generates its Keyboard Shortcuts
section from the same table, so help prose and key handling cannot drift (#893). Tests pin
every entry to its real dispatcher (this module and `lib.rs`).

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-tui --lib`.

## The blit pipeline: ViewModel → widget tree → claiming → paint

Every blit converts domain state into one owned ViewModel snapshot, then lays it out:

1. **`view_model::LiveViewModel`** is the blit-time snapshot — composer draft + cursor,
   completion rows (empty unless the draft is a slash command or `@` mention), status lines,
   live transcript lines, dialogs. `live_frame_sources` gathers the owned state;
   `plan_live_frame` plans the frame from it and is a pure function (assertable without a
   terminal).
2. **`view_model::project_message`** is a thin renderer adapter: it asks the `Message` trait for a
   WorkUnit snapshot with the current `ColorScheme`, then delegates the snapshot →
   `TranscriptNode` conversion to `finch_ui_model::project_work_unit`. Widgets never query
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
   separate once-per-id pipeline. Stage 4 (#1141): component-owned lines carry **styled spans**
   (`RenderedTranscriptLine.spans`, built from a `ComponentStylePalette` the renderer bridges
   from the user's `ColorScheme`), and two paint seams lower them to SGR — the live viewport
   assembly (`span_render::lower_rendered_line` at the frame's `push`) and the scrolled
   transcript region (`redraw_full_viewport_inner`). Span-free lines keep their legacy bytes;
   the plain `text` is what every measurement and the canonical record read, and the canonical
   commit path never sees spans (pinned by `test_component_spans_lower_to_sgr_at_the_live_frame_paint`).

## Span lowering is the render mode's job (#1141)

`span_render.rs` is the only conversation-side place that names SGR codes for spans: one
attribute run per span (bold, dim, fg, bg) closed by a reset, so unchanged styles render
byte-identically to the retired helpers. `component_style_palette(&ColorScheme)` is the one
engine boundary that meets the scheme — scheme-owned roles (progress, static-row colours) map
through it; the glyph vocabulary (⏺ ⎿ dim summaries) keeps the pre-migration fixed colours.
The DOM mode lowers the same spans to styled elements instead (below); neither mode is
compiled into the other.

## The widget vocabulary lives in `finch-ui-model` (#877/#882)

`Rect`, `Track`, `Axis`, `Widget`, `Layout`, `RenderedTranscriptLine`, `RowId`, `NodeRole`,
and the pure line-metric functions live in `finch-ui-model`, imported directly so a
component can build and claim a subtree without touching `crossterm` or the shadow buffer.
The engine keeps its stable `widgets` /
`shadow_buffer` paths as re-exports; new surface authors depend on the vocabulary directly.

## Component-owned messages (#882 stages 1–2; #1120 stage 3)

Every migrated typed message renders through its **component** via the generalized
`Message::component_view` accessor (stage 3 of `docs/TUI_DESIGN.md`):
`TuiRenderer::projected_message_lines` asks the trait for the `ComponentView` snapshot and hands
it to `finch_ui_model::component_lines` — the engine never matches on the message type (pinned
by `isolation::test_projection_path_never_matches_on_message_type`). Stage 3 completes the
model: the say turn (#882 stages 1–2) rides the same accessor, and `StaticMessage` (text is its
view), `ProgressMessage` (the bar/line from its VM; a committed row zero-claims out of the live
viewport through the canonical pipeline), `LiveToolMessage` (header + streaming content subwidget,
an empty content claims zero rows and the header carries the running `…`), and `OperationMessage`
(chrome `⏺ header…` plus the rows subwidget with per-row status glyphs, `⎿` per call) each own
their semantics in the component capsule. Component renderers emit plain text — glyphs carry the
semantics; the style-spans migration is stage 4. Say turns: `say_turn_lines` renders Generating as
one animated line, Running as the program source inline (arrived output bytes beneath it, never
hidden), and Completed as the output prose plus `(ran Ns)` — no Program source row, no Brain run
row, no result row, no card chrome. The stage-1 chrome (glyph + arrow + the `[0]` hitbox) is
deleted; the toggle hit target is the completed output region (semantic path `[1]`, clicked or
driven by F6/Enter), routing the opaque action to the message's `handle_transcript_action`, which
toggles `show_program` under the message's lock; the next frame re-renders from the mutated
ViewModel. The legacy source-group row does not render beside the card: `TuiRenderer::projected_lines`
pairs each say-VM unit with the adjacent completed Program-source unit whose response text is
byte-identical to the turn's program (`say_turn_consolidated_source_ids`) and suppresses that
row from the viewport only — byte identity holds by construction in every producer path and a
mismatch suppresses nothing, so the rule fails toward rendering more, never less. The canonical
record is untouched (`commit_complete_messages` still projects unmigrated rows through
`project_message`, fully expanded): the raw program and the say bytes still spool exactly once,
and the pinned canonical-commit invariants pass as-is. The renderer's RowId-keyed open-set maps
never hold component rows: they register in `AccordionState::component_regions` for routing
only. Unmigrated rows (non-say WorkUnit presentations — the open stage-2 scope) keep the maps
and the legacy projection path.

`MemoryRecalledMessage` rides the same component-owned disclosure mechanism (#1235): each
recalled-memory row's full text is collapsed behind its identity/summary line by default and
expands on click, addressed by semantic path `[row index]` (one toggle target per row, unlike
the say turn's single `[1]` output region). `MemoryRecalledMessage` retains one `expanded: bool`
per row behind its own lock; `transcript_action`/`handle_transcript_action` route through
`ToggleMemoryRow(index)`, the same opaque-`ComponentAction` pattern as `ToggleProgram`. Because a
second component type now pins a keyboard direction (Left/Right's `want`), `dispatch_component_disclosure`'s
already-correct-state check reads `Message::component_view` and matches on the `ComponentView`
variant (`Say` → `show_program`, `MemoryRecalled` → the row at `row_id.path`) instead of calling
the say-specific `say_turn_view` accessor directly — a future component-owned disclosure type
extends this same match arm rather than inventing a second dispatch path.

## Assistant prose markdown renders in the viewport (#756)

`crates/finch-ui-model/src/markdown.rs` (private) is a deliberately bounded inline-subset parser — fenced code blocks,
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

## The conversation ScrollView owns reading (#806, #897)

The transcript region — the root column's `Flex` `TRANSCRIPT` claim, the leftover frame
under the bottom chrome — is an in-app scroll view (`scroll_view.rs`). Renderer state is one
offset from the bottom of the scroll content (0 = follow mode). The scroll content is the
**projected rendered-line union** (#897): every message retained by the output port — the
canonical-committed prefix **and** the live uncommitted suffix — projected at paint time by
the same component-owned projection the viewport paints (`projected_lines`, say-turn
consolidation included); `printed_ids` never filters scroll content and stays the
canonical-commit exactly-once record. The window is derived at paint time by
`TranscriptScrollView::derive_window` (which also anchors the offset against content
growth while scrolled, so streaming appends and commits never drag a scrolled reader, and
stores the quantised honest offset), shared by the full-viewport repaint, the retained
hit-region rebuild, and the live frame's clip. The two painted surfaces divide the window
at the committed/live boundary: the transcript region paints the tail of the union's
committed portion above the hidden tail, and the live area paints only the suffix's lines
above the hidden tail (`union[live_start..split]`), so a scrolled reader sees one
contiguous window and the live suffix scrolls away like native scrollback; in follow mode
(offset 0) nothing is clipped and frames are byte-identical to the unscrolled shape.
Wheel ticks land on the ScrollView inside the claim, on a nested tool-result control
inside its rect, and on nothing over the bottom chrome; the claim is the wheel hitbox,
stored from `frame.rects` by the same rebuild that rebuilds the accordion regions.
Step sizes are the ScrollView's own (#897): a wheel tick moves `TRANSCRIPT_WHEEL_STEP_LINES`
(3) rows and PageUp/PageDown move one page of the visible pane
(`TranscriptScrollView::page_step`), independent of the bounded tool-result viewport's
`WHEEL_STEP_LINES` (1) and `PAGE_STEP_LINES` (4), which keep serving the tool viewport and
the expanded surface only. PageUp/PageDown scroll it from the keyboard, independent of
mouse tracking. Mouse tracking is held by default (`mouse_capture.rs`) — the #441
release-on-first-wheel hybrid is retired, native history stays the copyable record via
`canonical_commit`, and while scrolled up a commit anchors the window instead of dragging
the reader.

## Click-drag transcript text selection (#221)

Mouse capture is held by default (#806, above), so the terminal never gets a native click-drag
selection — `selection.rs` is Finch's own in-app replacement, dispatched from
`TuiRenderer::handle_mouse_to`'s non-wheel branch. A `Down(Left)` on an existing click hitbox
(`point_is_on_click_hitbox`: a tool-viewport control, a component disclosure region, or a legacy
accordion `hit_region_at`) keeps today's immediate toggle-on-press behavior untouched and never
starts a selection; a `Down(Left)` anywhere else in the transcript claim only stashes a press
candidate — nothing highlights until a `Drag(Left)` actually moves, so a plain click still selects
nothing and a press-then-drag that started on a hit region never also selects the row underneath it.
`Up(Left)` finalizes the selection (it stays highlighted and copyable — matching Claude Code's own
released-selection behavior) and best-effort copies the text to the system clipboard through
`arboard` (`copy_selection_to_clipboard`), the same crate already used for the OAuth device-code
copy in `grok_auth.rs`/`chatgpt_auth.rs`; a clipboard failure never clears the selection.

`SelectionIndex` (rebuilt every frame in `rebuild_transcript_hit_regions`, from the same
`combined` `RenderedTranscriptLine`s and `plan.transcript_top` the hit regions use) only indexes
rows that occupy exactly one physical terminal row — a wrapped multi-row logical line is not
indexed, so a drag simply has a gap there; wrapping-aware selection is a documented follow-up, not
something guessed at. The highlight itself lowers through the normal `Span`/`SpanStyle` path
(`span_render::selection_highlight_style`, `lower_span`) exactly like every other transcript
style — never raw SGR in a renderer — but paints as a small targeted overlay
(`paint_selection_overlay`, bracketed by `SavePosition`/`RestorePosition`) after the normal frame
write, not as a third span-lowering seam over component content: it never touches
`RenderedTranscriptLine`/component styling, and a row it touches reverts to plain text plus the
highlight background until the next full content redraw restores its real styling. A finalized
selection is cleared by the simplest rule that matches "redraw invalidates it": unconditionally, at
the top of `redraw_full_viewport_inner` (a new committed message, an explicit scroll, or a
resize) — a drag still in progress is never routed through that function, so an ordinary drag tick
never trips the clear.

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
submit time, and an explicit `Answer:` line — through `TuiOutputPort`, so the standard
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

**Wizard props carry styled spans (#1141).** `WizardView`/`WizardCard`/section lines hold
`WizardLine` segments (text + `WizardColor` fg/bg + bold/dim); the only escape codes in the
wizard surface live in the host's lowering (`lower_wizard_line`), and the view builders
construct no bytes — pinned by
`test_wizard_view_builders_construct_no_sgr_bytes`. The #1140 symptoms ride the restored
background channel: selected rows paint `wizard_selected` (bold bright-white on black, the
pre-migration contrast), the active tab paints bold magenta on black from the
`selected_tab` marker prop (`test_active_tab_marker_prop_selects_the_highlighted_tab`,
`test_selected_rows_carry_selection_contrast_beyond_the_prefix`), and the context-lines
spinner shows its value with its ◀/▶ keys advertised on every platform
(`test_context_lines_spinner_value_is_visible_keys_adjust_and_keys_are_advertised`).

## The DOM manifest lowering (#1141 part 2)

`dom_manifest.rs` lowers the same widget tree and component snapshots to the versioned
`UiManifest { manifest_version, root: DynamicUiNode }` — the wire contract the Tauri client
(#808) consumes. `element_type` comes from the component (`SayTurnCard`, `StaticText`,
`Progress`, `LiveTool`, `Operation`, plus the engine vocabulary `Stack`/`Text`/`Viewport`/
`DialogCard`/`Rule`/`Completions`/`Composer`); ids are **derived** (message uuid, or
`{uuid}#{path}` matching the `RowId` semantic paths — the say card's output child is
`{uuid}#1`, the toggle hit target). Props are JSON values (`BTreeMap` keeps golden JSON
deterministic); components never write HTML. The contract is documented and versioned in
[docs/UI_MANIFEST.md](../../docs/UI_MANIFEST.md); TS types are generated by ts-rs into
`bindings/dom/` (committed; regenerated on every lib-test run) and pinned by the golden
test `tests/ui_manifest.rs` (say card JSON + serde round-trip + derived ids).

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
| `finch_ui_model::TranscriptNode` | label, body, children, role, default disclosure | WorkUnit snapshot, projected by `finch_ui_model::project_work_unit`; adapted from `Message` in `view_model::project_message` |
| `finch_ui_model::SayTurnView` | status, program, output, elapsed, toggle state | WorkUnit `say_turn_view()`, projected by `finch_ui_model::say_turn_lines` |
| [`ActivityRow`](src/activity.rs) | indented status text | todos / agent tasks, in `cli::repl_event::activity_view` |
| `Dialog::tool_approval(name, summary)` | a name and a summary line | Finch `ToolUse`, in `cli::repl_event::tool_display::tool_approval_dialog` |
| `QuestionView` / `QuestionOptionView` | question text, tab heading, options, selection mode, and optional preview | `AskUserQuestion` request in `cli::llm_dialogs`; converted before `TabbedDialog::new` |
| `MentionCandidate` / `MentionSubmission` | speakable picker rows, insertion tokens, and provider-independent selected bytes | `context::mention`, through the injected CLI adapter in `cli::mention_session` |

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

**Lower crates this directory names in production, and why they stay:**

- **`finch_theme::ColorScheme`** — shared colour vocabulary from the extracted leaf crate,
  re-exported so callers can use the flat `finch_tui::ColorScheme` facade.
- **`finch_diff`** — bounded, terminal-safe diff summaries and dialog sanitation from the
  extracted leaf crate. `finch-messages` separately uses it to construct WorkUnit snapshots.
- **`finch_messages` and `finch_ui_model`** — the renderer reads typed message snapshots and
  delegates pure WorkUnit projection to the lower presentation crate. The AskUserQuestion wire
  contract and answer annotations stay in `cli::llm_dialogs`; the renderer sees only `QuestionView`.
  Command-completion metadata is owned
  directly by this capsule in `command_autocomplete.rs`; its types and `AutocompleteState` remain crate-visible only, not
  facade exports. The unused contextual-suggestion subsystem was removed. `shadow_buffer.rs`
  has no production message dependency; its band-style tests drive `write_line` with a user-message
  style, and the uncalled `render_messages` path was deleted.
- **`finch_ipc::finch_ipc_capnp`** — encodes the quit control message consumed by the
  application-owned watcher. The TUI does not own the socket or the quit policy; it receives
  the sender and an editor-activity query from its caller.

**Boundary checks:** application-owned `ToolUse` approval-assembly fixtures live with
`cli::repl_event::tool_display` tests. Dialog tests here exercise terminal layout from
renderer-owned `Dialog` values, and test-only output/status ports avoid a dependency on
the root CLI adapters.
`test_tui_production_does_not_name_finch_tools_or_runtime` fails if production source grows a
`crate::tools` or `crate::runtime` name, and
`test_tui_production_does_not_name_finch_poset` keeps the deleted write-only Poset edge from
returning. `test_tui_production_does_not_name_project_context` keeps filesystem discovery,
ignore/budget policy, and attachment snapshots behind the injected `MentionPort`; its application
adapter is [`src/cli/mention_session.rs`](../../src/cli/mention_session.rs).
`test_tui_production_does_not_name_ask_user_question_wire_schema` keeps the tool request and
response in the CLI while the renderer consumes only `QuestionView`.
`test_tui_production_does_not_reach_up_for_owned_completion_state` keeps command completion
under this facade instead of reaching through `crate::cli`.
`test_scanner_would_fail_if_runtime_returned_to_draw_live_area` and
`test_scanner_would_fail_if_tools_returned_to_tool_approval` fail if the scanner can no longer
see those production functions; the removed file viewer is no longer a scan target.
