# messages capsule: client-local conversation entries

Supplements the root [AGENTS.md](../../../AGENTS.md). The [README](README.md) traces the query
processor and TUI callers; [`mod.rs`](mod.rs) is the callable facade.

**Owns:** typed messages, their shared read contract, mutable `WorkUnit` turn lifecycle, and the
snapshot assembled for each render. A WorkUnit is one generation run, never a widget kind. This
module does not own the durable Brain journal, provider/tool execution, terminal layout, or
renderer disclosure state.

**Dependencies and direction:** snapshot types and pure projection belong to `finch-ui-model`;
color roles come directly from `finch-theme`, and bounded structured file diffs from
`finch-diff`. Production message code must not reach back through root `config`, `theme`, or
`cli::diff` compatibility aliases. Model-loader
progress adaptation belongs in application-owned `cli::output_manager`, not this message model.
Brain, runtime, provider, and tool layers must not depend upward on this message model;
application adapters construct messages at the conversation boundary.

The production outgoing seam is now only those three extracted presentation crates. Some
message tests still call root TUI projection and REPL tool-display helpers; move or replace
those test-only reverse edges with equivalent integration coverage before a mechanical
message-model crate extraction.

**Invariants and lifetimes:** a `MessageId` remains stable across streaming updates. WorkUnit
row paths are append-only semantic ancestry; never reuse or reorder a path segment. The same
shared unit may be read while the event loop appends output, so preserve the existing lock and
snapshot discipline. A complete transcript is canonical text for copying and permanent
scrollback; renderer disclosure may change visible rows but must not change that text. A say-turn
component action mutates its owning ViewModel under the message lock; unmigrated rows keep the
renderer-owned `RowId` open set. Brain replay remains authoritative for durable state.

**Extension rules:** add a concrete message only for a real application producer and renderer
need. Keep pure snapshot-to-widget conversion in `finch-ui-model`; do not put terminal I/O or
provider dispatch here. Child modules stay private; add a flat re-export only when an actual
caller needs it. Do not add whole-module exports or recreate a generated `INTERFACE.md`.

**Focused tests:**
`./scripts/test_brains.sh cargo test --lib -- cli::messages::` for lifecycle and snapshot
assembly, `./scripts/test_brains.sh cargo test -p finch-ui-model` for pure projection, and
`./scripts/test_brains.sh cargo test --lib -- cli::tui::` when message rendering changes.
Run the supervised workspace suite for facade or persisted-shape changes.
