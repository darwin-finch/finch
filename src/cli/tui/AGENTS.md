# tui capsule: terminal renderer and widgets

Supplements the root [`AGENTS.md`](../../../CLAUDE.md), which still applies in full.

**Owns** `src/cli/tui/`: the interactive terminal renderer (`TuiRenderer`), dialogs, the live
area, scrollback, the ViewModel projection, the claiming widget tree, disclosure (accordion),
activity rows, and graph *view types*.
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
   width-conditional rails (#805, #809, #810).
4. Painting stays line-based on the claimed rects; native `canonical_commit` remains the
   separate once-per-id pipeline.

## View types the renderer owns

The same inversion the todo list and child-agent rows already use (`activity.rs`): the renderer
draws a view, and the caller converts.

| View | What the renderer needs | Converted from |
|------|-------------------------|----------------|
| `view_model::TranscriptNode` | label, body, children, role, default disclosure | WorkUnit `domain_view()`, in `view_model::project_message` |
| [`activity::ActivityRow`](activity.rs) | indented status text | todos / agent tasks, in `cli::repl_event::activity_view` |
| [`GraphView`](graph.rs) | nodes, edges, camera | Finch `Poset`, via unused `graph_view_from_poset` |
| `Dialog::tool_approval(name, summary)` | a name and a summary line | Finch `ToolUse`, in `cli::repl_event::tool_display::tool_approval_dialog` |
| [`cell_format::workbook_cell_to_string`](cell_format.rs) | one cell as text | calamine `Data`, inside `spreadsheet_preview_rows` |

When `active_dialog` first occupies the live surface, `draw_live_area` writes one terminal bell
(`\x07`). Redraws of the same pending overlay stay silent. OS notifications, duration-threshold
run-complete toasts, and a config off-switch remain follow-up on #752 (notify on attention-needed).

## Disclosure and focus are renderer state

`AccordionState` (in `accordion.rs`) is the single owner of open/closed, keyed by the stable
`RowId` (message id + append-only semantic path). A row's default is a projection-time prop
(`TranscriptNode::default_open`, derived from domain status); the user's toggle overrides it and
survives re-projection, streaming appends, terminal reflow, and reconnects. Completing a run
cannot collapse a result. Leaf lines render exactly the ViewModel's label — which carries the
status glyph — and never invent a `•` bullet or sniff glyph characters (#821).

Splitting the poset widget out of the framework (rather than generalising it) remains open.
Production does **not** convert at `set_poset` and does **not** draw `GraphView`:

- `TuiRenderer::set_poset` stores `Arc<Mutex<Poset>>` and returns.
- `graph_view_from_poset` / `poset_to_forth_lines` are the view-shaped renderer and are unused
  (`dead_code`). Tests call them.
- `draw_poset_overlay` paints the user-defined `check` word from `corner`, not the graph.

Finch's `Poset` stays on that field because `EventLoop` still hands the shared mutex in and this
capsule must not rewrite `repl_event`. Wiring `GraphView` at `set_poset` or `draw_poset_overlay`
is a behaviour change, not a documentation fix.

## Deliberate remaining production references

Acceptance for #623 (terminal framework standing alone) is: no production reference to any other
module of this crate, **or** the remaining ones are written down here with the reason.

**Finch modules this directory still names in production, and why they stay:**

- **`crate::poset::Poset` in `mod.rs` only** — the `poset` field, `TuiRenderer::set_poset`, and
  the unused `graph_view_from_poset` helper. The event loop shares `Arc<Mutex<Poset>>` with the
  executor. Do not add new `crate::poset` names outside `mod.rs`
  (`test_tui_production_keeps_finch_poset_at_the_injection_boundary`).
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
- **`crate::poset` in `mod.rs` tests** — one conversion test builds a Finch poset and checks
  `graph_view_from_poset` preserves the Forth projection. Other graph tests use `GraphView`.

`test_tui_production_does_not_name_finch_tools_or_runtime` fails if production source grows a
`crate::tools` or `crate::runtime` name. `test_scanner_would_fail_if_runtime_returned_to_spreadsheet_preview_rows`
and `test_scanner_would_fail_if_tools_returned_to_tool_approval` fail if the scanner can no
longer see those production functions (the original leak sites).
