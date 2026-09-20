# messages: typed messages and WorkUnit domain snapshots

This is Finch's typed message system: the `Message` trait, its concrete types, and `WorkUnit` —
one AI generation turn with its tool rows, program source/output, and child-agent lifecycle rows.
Everything here is domain data, never a widget kind or a presentation choice; the renderer projects
it into widget props exactly once per frame from two plain-data snapshots (`work_unit_head`,
`work_unit_view`). It exists as a layer separate from the renderer so the domain model never
depends upward on `cli::tui` or `cli::components`.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).

## Further documentation

[`docs/TUI_DESIGN.md`](../../../docs/TUI_DESIGN.md) — the say-turn component ViewModel this module
carries data for.
