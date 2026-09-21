# Finch presentation model

This crate holds terminal-independent presentation data and pure projection functions shared by
message producers and renderers. It gives retained messages and transcript rows stable identity,
projects WorkUnit snapshots into semantic nodes, formats bounded assistant prose, and measures
and claims layout cells. It does not own message mutation, component action dispatch, input,
terminal lifecycle, or painting.

When a WorkUnit changes, `crates/finch-messages/src/work_unit.rs` retains the domain event and constructs
plain `WorkUnitView` and `WorkUnitHead` snapshots. It also retains the say-turn
`WorkUnitViewModel` while streaming and completing a turn. The message layer owns the mutation;
this crate owns the data shapes and pure say-turn line projection.

When the live TUI blits, `src/cli/tui/view_model.rs` asks the message trait for a snapshot and
calls `project_work_unit`. The renderer then builds its widget tree and claims frame rectangles
using this crate's presentation vocabulary. Disclosure and focus remain renderer state; terminal
painting stays in the TUI.

Read [AGENTS.md](AGENTS.md) for invariants and focused tests, [src/lib.rs](src/lib.rs) for the
flat facade, and `cargo doc -p finch-ui-model --no-deps --open` for methods on exported types.
