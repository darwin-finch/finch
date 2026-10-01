# Finch messages agent contract

Supplements the root [AGENTS.md](../../AGENTS.md). The [README](README.md) traces the query
processor and TUI callers; [`src/lib.rs`](src/lib.rs) is the callable facade.

**Owns:** typed messages, their shared read contract, mutable `WorkUnit` turn lifecycle, and the
snapshot assembled for each render. A WorkUnit is one generation run, never a widget kind. Migrated
message types (`WorkUnit` say turns, `StaticMessage`, `ProgressMessage`, `LiveToolMessage`,
`OperationMessage`, `MemoryRecalledMessage`) answer `Message::component_view` by constructing
their component snapshot from retained state under the existing lock(s) — no new lock, no
OutputManager ownership change; the renderer never matches on the message type. This module does
not own the durable Brain journal, provider/tool execution, terminal layout, or renderer
disclosure state.

**Dependencies and direction:** snapshot types and pure projection belong to `finch-ui-model`;
color roles come directly from `finch-theme`, and bounded structured file diffs from
`finch-diff`. Production message code must not reach back through root `config`, `theme`, or
`cli::diff` compatibility aliases. Model-loader
progress adaptation belongs in application-owned `cli::output_manager`, not this message model.
Brain, runtime, provider, and tool layers must not depend upward on this message model;
application adapters construct messages at the conversation boundary.

Those are this crate's only Finch-crate dependencies. Its tests exercise `finch-ui-model`
projection through the `Message` snapshot; the root TUI adapter has its own tests in
`cli::tui::view_model`, and the REPL tool-result/retained-message integration test lives with
`cli::repl_event::tool_display`. Keep those cross-layer tests at the root, not in this crate,
to avoid a dev-dependency cycle.

**Invariants and lifetimes:** a `MessageId` remains stable across streaming updates. WorkUnit
row paths are append-only semantic ancestry; never reuse or reorder a path segment. The same
shared unit may be read while the event loop appends output, so preserve the existing lock and
snapshot discipline. When a run or turn reaches a terminal outcome, every still-`Running` child
row (tool rows, approvals, and child-agent rows and their tools) must resolve with that outcome
instead of freezing at its last in-flight status — `WorkUnit::resolve_running_rows_with_run_outcome`
(this file) is the single terminalization path callers use for this, wired from
`apply_brain_run_status` and the `QueryFailed` dispatch handler in
`src/cli/repl_event/event_loop.rs`/`dispatch.rs` (#910). `BrainRunStatus::Interrupted` is
deliberately excluded from `is_terminal()` (`crates/finch-brain/src/run/mod.rs`) because an
interrupted run may resume, so its child rows intentionally stay live rather than being
force-resolved. `test_run_terminal_status_resolves_stuck_child_rows_on_disconnect` in
`src/cli/repl_event/event_loop/tests.rs` replays the issue's real disconnect transcript capture. A complete transcript is canonical text for copying and permanent
scrollback; renderer disclosure may change visible rows but must not change that text. A say-turn
component action mutates its owning ViewModel under the message lock; unmigrated rows keep the
renderer-owned `RowId` open set. `MemoryRecalledMessage` (#1235) rides the same component-owned
disclosure pattern: `header`/`rows` are immutable once constructed, but each row's `expanded`
flag lives in its own `RwLock<Vec<bool>>`, collapsed by default, and `ToggleMemoryRow` (addressed
by row index through `transcript_action`/`handle_transcript_action`) flips one row's flag under
that lock — `complete_transcript`/`format` are unaffected, so native scrollback still carries the
full recalled text regardless of what the live viewport has collapsed. Migrated messages answer
`Message::component_view` (stage 3 of
docs/TUI_DESIGN.md, #1120) by constructing their component from retained state under the
existing lock(s) — no new lock, no OutputManager ownership change; the renderer never matches
on the message type. Say turns ride the same accessor (`ComponentView::Say`); `say_turn_view`
stays for the consolidated-source pairing helper and the disclosure-direction read. Brain
replay remains authoritative for durable state.

`WorkUnitPresentation::Interactive` is the client-local semantic projection of a durable
named-Brain Interactive run. It may retain program, tool, approval, and terminal child rows, but
its root is assistant conversation and must never expose the run UUID as presentation text.

**Extension rules:** add a concrete message only for a real application producer and renderer
need. Keep pure snapshot-to-widget conversion in `finch-ui-model`; do not put terminal I/O or
provider dispatch here. Child modules stay private; add a flat re-export only when an actual
caller needs it. Do not add whole-module exports or recreate a generated `INTERFACE.md`.

**Public surface audit (#1063):** a cross-crate, alias-aware grep sweep (workspace-wide by bare
identifier, following every re-export chain — `crates/finch-messages/src/lib.rs`,
`src/cli/messages/mod.rs`, and `src/cli/mod.rs` — not a definition-scoped LSP reference search,
which had produced confirmed false negatives on `WorkUnit::queue_agent_activity` and `MessageRef`)
narrowed `OperationRow`, `OperationRowStatus`, `StaticMessageType`, `ToggleMemoryRow`,
`ToolExecutionMessage`, `ToggleProgram`, `WorkRow`, `ComponentAction::{new,downcast_ref}`,
`WorkUnit::{set_thinking,complete_row_with_diff}`, and `LiveToolMessage::{set_content,set_failed}`
to `pub(crate)` after confirming zero external callers by any path. `ToolExecutionMessage`'s
`append_stdout`/`append_stderr`/`set_exit_code`/`set_failed` and `StaticMessage`'s
`success`/`warning` (plus the now-unreachable `StaticMessageType::{Success,Warning}` variants and
their `format`/`component_view` match arms) were deleted outright: zero callers anywhere,
including this crate's own tests. `MessageRef`, `WorkUnit::{is_assistant_prose,
append_row_body_line}`, and the seven `WorkUnit` methods widened during extraction
(`spawn_agent_row_indices`, `is_activity_presentation`, `queue_agent_activity`,
`start_agent_activity`, `start_agent_tool`, `complete_agent_tool`, `finish_agent_activity`) all
have confirmed real external callers and are unchanged. `StreamingResponseMessage::set_thinking`
has zero callers anywhere (not even this crate's own tests) yet was left exactly as `pub` rather
than deleted: it is the only writer of a `thinking` field that `format()` actively reads to render
the "[thinking…]" streaming indicator, so an empty caller list here looks like an unwired product
feature, not dead API surface — a maintainer decision, not one to make silently inside an API
audit. Full findings: issue #1063.

**Focused tests:**
`./scripts/test_brains.sh cargo test -p finch-messages --lib` for lifecycle and snapshot
assembly, `./scripts/test_brains.sh cargo test -p finch-ui-model` for pure projection, and
`./scripts/test_brains.sh cargo test -p finch --lib cli::tui::` when message rendering changes.
Run the supervised workspace suite for facade or persisted-shape changes.
