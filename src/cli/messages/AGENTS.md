# messages capsule: typed messages and WorkUnit domain snapshots

Supplements the root [`AGENTS.md`](../../../CLAUDE.md), which still applies in full.

**What this is.** The typed message system: the `Message` trait (id, format, status,
complete transcript), its concrete types (`concrete.rs`), and `WorkUnit` (`work_unit.rs`) —
one AI generation turn with tool rows, program source/output presentations, and child-agent
lifecycle rows. Everything here is **domain data**: a WorkUnit is one run, never a widget kind.

**No presentation surface lives here.** Since #805 the old `TranscriptRow` projection tree is
gone. WorkUnit snapshot types and the pure snapshot → `TranscriptNode` conversion live in
`finch-ui-model`; this module constructs and re-exports the snapshots for source compatibility.
Disclosure state remains renderer-owned and keyed by the projected stable row identities.

**What the renderer reads.** Two domain snapshots (plain data, safe to expose):

- `Message::work_unit_head() -> Option<WorkUnitHead>` — presentation class, status, body text;
  for classify/filter consumers (live-message replacement).
- `Message::work_unit_view(colors) -> Option<WorkUnitView>` — the full blit-time snapshot:
  rows with labels, statuses, bodies, and diffs pre-rendered to display lines.
  `finch_ui_model::project_work_unit` projects it into widget props once per frame; the root
  TUI adapter supplies the message trait and colour scheme.

Both default to `None` on the trait; only WorkUnit overrides them. Disclosure persistence is not
a message concern: the renderer's open set is keyed by stable row identity (message id +
append-only path), so completing a run or reconnecting cannot lose a choice.

**The say-turn component ViewModel (#882).** A WorkUnit can carry a component-owned ViewModel
(`WorkUnitViewModel`: status, program, output, `show_program`) for successful untitled `say`
turns — stages 1–2 of `docs/TUI_DESIGN.md`, one representation per state. The producer creates
it (`begin_say_turn`) where the say turn is born and already holds the wire source; streaming
appends and the completion path update it under the same lock that mutates the unit. The pure
snapshot types and state-based line projection live in `finch-ui-model`; this module re-exports
the snapshot types for source compatibility. The action payload (`ToggleProgram`, carried
opaquely as `ComponentAction`) remains beside the message lifecycle it mutates. The `Message`
trait exposes the snapshot (`say_turn_view`, carrying full-resolution elapsed for the animated generating state),
the row action (`transcript_action` — the completed output region, semantic path `[1]`, since
the stage-1 chrome's `[0]` retired with the chrome), and the handle
(`handle_transcript_action`); rows without a ViewModel keep the renderer's RowId-keyed
disclosure maps.

**Stable identity.** `MessageId` is a UUID; row paths are append-only semantic ancestry
(unit, call index, input/output). Never reuse or reorder a path segment.

**Testing.** Message lifecycle and snapshot-construction tests live with `work_unit.rs`.
Projection-shape and markdown tests live with `finch-ui-model`; root integration tests exercise
the thin `Message`/`ColorScheme` adapter. Focused tests:
`./scripts/test_brains.sh cargo test --lib -- cli::messages::` and
`./scripts/test_brains.sh cargo test -p finch-ui-model`.
