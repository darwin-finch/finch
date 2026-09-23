# ui-model capsule: terminal-independent application presentation vocabulary

Supplements the root [`AGENTS.md`](../../AGENTS.md), which still applies in full.

**Owns** `crates/finch-ui-model/src/`: stable retained-message and row identity, WorkUnit
presentation snapshots, semantic transcript nodes, terminal-independent widget data, pure
component and WorkUnit projections, the generalized component view (`component.rs`: every
migrated typed message yields its `ComponentView`; the per-variant dispatch lives here, never
in the engine), the style-span vocabulary (`span.rs`: `Span`/`SpanStyle`/`SpanColor` —
semantic segments, no terminal bytes; `RenderedTranscriptLine.spans` concatenates to `text`
exactly, and the `ComponentStylePalette` the engine injects carries the scheme-owned styles),
bounded assistant-prose markdown, line measurement, and the claiming layout pass. These are
plain data and pure functions shared by message producers and render engines.
The message layer retains and mutates domain state and constructs snapshots; it does not own
their presentation projection. Component renderers construct no SGR — pinned by
`test_component_renderers_construct_no_sgr_bytes`; styles are palette values the engines
lower (stage 4 of docs/TUI_DESIGN.md, #1141).

**Does not own** message lifecycle or synchronization, component action dispatch, terminal
lifecycle, `crossterm`, shadow-buffer painting, input handling, or application composition. Those
layers depend on this capsule; this capsule does not depend back on them.

**Boundary:** the [README](README.md) traces message-producer and TUI callers.
[`src/lib.rs`](src/lib.rs) is the complete flat facade; rustdoc supplies exact methods on
exported types. There are no public child-module paths. Do not recreate a signature catalog.

**Dependencies:** none of Finch's other subsystems. `unicode-width` supplies the same terminal-cell
tables used before extraction, and `uuid` supplies stable message identity. Child modules are
private; callers use only the flat `pub use` facade in `lib.rs`.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-ui-model`.
