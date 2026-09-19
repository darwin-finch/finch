# messages capsule: typed messages and WorkUnit domain snapshots

Supplements the root [`AGENTS.md`](../../../CLAUDE.md), which still applies in full.

**What this is.** The typed message system: the `Message` trait (id, format, status,
complete transcript), its concrete types (`concrete.rs`), and `WorkUnit` (`work_unit.rs`) —
one AI generation turn with tool rows, program source/output presentations, and child-agent
lifecycle rows. Everything here is **domain data**: a WorkUnit is one run, never a widget kind.

**No presentation surface lives here.** Since #805 the old `TranscriptRow` projection tree is
gone; disclosure (`default_expanded`), row roles, and open/closed state belong to the renderer's
ViewModel (`src/cli/tui/view_model.rs`), which is the one domain → widget conversion.

**What the renderer reads.** Two domain snapshots (plain data, safe to expose):

- `Message::work_unit_head() -> Option<WorkUnitHead>` — presentation class, status, body text;
  for classify/filter consumers (live-message replacement).
- `Message::work_unit_view(colors) -> Option<WorkUnitView>` — the full blit-time snapshot:
  rows with labels, statuses, bodies, and diffs pre-rendered to display lines. The renderer's
  ViewModel projects it into widget props once per frame.

Both default to `None` on the trait; only WorkUnit overrides them. Disclosure persistence is not
a message concern: the renderer's open set is keyed by stable row identity (message id +
append-only path), so completing a run or reconnecting cannot lose a choice.

**The say-turn component ViewModel (#882).** A WorkUnit can carry a component-owned ViewModel
(`WorkUnitViewModel`: status, program, output, `show_program`) for successful untitled `say`
turns — stages 1–2 of `docs/TUI_DESIGN.md`, one representation per state. The producer creates
it (`begin_say_turn`) where the say turn is born and already holds the wire source; streaming
appends and the completion path update it under the same lock that mutates the unit. The types
and the say component's action payload (`ToggleProgram`, carried opaquely as
`ComponentAction`) live here so the domain never depends upward on the component layer; the
state-based renderer and subwidgets live in `cli::components`. The `Message` trait exposes the
snapshot (`say_turn_view`, carrying full-resolution elapsed for the animated generating state),
the row action (`transcript_action` — the completed output region, semantic path `[1]`, since
the stage-1 chrome's `[0]` retired with the chrome), and the handle
(`handle_transcript_action`); rows without a ViewModel keep the renderer's RowId-keyed
disclosure maps.

**Stable identity.** `MessageId` is a UUID; row paths are append-only semantic ancestry
(unit, call index, input/output). Never reuse or reorder a path segment.

**Testing.** Projection-shape tests (labels, glyphs, disclosure defaults) live in
`work_unit.rs`'s test module and drive the ViewModel projector
(`crate::cli::tui::view_model::try_project_for_test`) so the coverage stays on the real
conversion path. Focused tests:
`./scripts/test_brains.sh cargo test --lib -- cli::messages::`.
