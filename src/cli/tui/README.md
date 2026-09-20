# tui: terminal renderer and widgets

This is Finch's interactive terminal renderer: the live area, dialogs, scrollback, the ViewModel
projection, the claiming widget tree, and the conversation scroll view. It exists as a boundary
that can draw without naming Finch's poset, tool, or runtime vocabularies — domain state comes in
as plain snapshots, and this module's only job is turning those into terminal cells.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).

## Further documentation

[`ARCHITECTURE.md`](ARCHITECTURE.md) — the renderer's blit pipeline and history model. Note:
`DESIGN.md` flags this document as describing a removed renderer variant (#442) — treat it as
partially stale until repaired.
