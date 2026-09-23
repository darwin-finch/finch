# Finch presentation model

This crate holds terminal-independent presentation data and pure projection functions shared by
message producers and renderers. It gives retained messages and transcript rows stable identity,
projects WorkUnit snapshots into semantic nodes, holds the generalized component view — every
migrated typed message yields its `ComponentView` through the `Message` trait, and this crate
renders the snapshot into claimed lines (`component_lines`; stage 3 of `docs/TUI_DESIGN.md`,
the say turn plus `StaticMessage`, `ProgressMessage`, `LiveToolMessage`, and `OperationMessage`)
— formats bounded assistant prose, and measures and claims layout cells. It does not own message
mutation, component action dispatch, input, terminal lifecycle, or painting.

When a WorkUnit changes, `crates/finch-messages/src/work_unit.rs` retains the domain event and constructs
plain `WorkUnitView` and `WorkUnitHead` snapshots. It also retains the say-turn
`WorkUnitViewModel` while streaming and completing a turn. The message layer owns the mutation;
this crate owns the data shapes and pure say-turn line projection.

When the live TUI blits, `crates/finch-tui/src/view_model.rs` asks the message trait for a snapshot and
calls `project_work_unit`; migrated messages answer the generalized `component_view` accessor
instead, and the renderer hands that snapshot to `component_lines` together with a
`ComponentStylePalette` built from the user's `ColorScheme` (stage 4). The renderer then builds its
widget tree and claims frame rectangles using this crate's presentation vocabulary. Disclosure
and focus remain renderer state; terminal painting stays in the TUI.

Stage 4 (#1141): component lines carry **styled spans** (`span.rs` — `Span`,
`SpanStyle`, `SpanColor`) alongside their plain `text`; the spans concatenate
to the text exactly, so measurement and the canonical record share one
content. Components never construct terminal bytes; render engines lower the
spans themselves (terminal → SGR at paint, DOM → styled elements), which is
what makes "one tree, two lowerings" true.

Read [AGENTS.md](AGENTS.md) for invariants and focused tests, [src/lib.rs](src/lib.rs) for the
flat facade, and `cargo doc -p finch-ui-model --no-deps --open` for methods on exported types.
