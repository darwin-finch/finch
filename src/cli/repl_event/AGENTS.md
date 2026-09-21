# repl_event capsule: the interactive REPL event loop

Supplements the root [`AGENTS.md`](../../../CLAUDE.md), which still applies in full.

**What this is.** The machinery behind the interactive REPL. A Tokio `select!` in
`EventLoop::run` reads user input, provider output, and Brain traffic, turns each into a
`ReplEvent`, and dispatches it. The selected generator is called by `query_processor`, and
tool execution is coordinated here through [`crate::tools::ToolLoop`] (shared with the
scheduler): stream events are observed, then admitted at most once.
`finch-conversation` owns the committed provider history and staged tool-round ledger; the
event loop owns the admission, checkpoint, and continuation decisions around those transitions.

**Boundary and dependencies.** The [README](README.md) traces construction by `Repl` and
event projection by the MemTree console. `mod.rs` is the facade; new callers should use its
flat exports rather than reaching through child modules. `memory_commitment` is private: the
REPL obtains its committed-set writer, target, receiver, record type, and constructor through
named facade exports. Other `pub mod` paths (notably `parts`, `brain_selection`, and
`tool_display`) still expose implementation paths to sibling CLI code; repair them in bounded
follow-ups rather than adding new child-path imports. The application injects provider,
tool, Brain, and UI dependencies; this module may coordinate them but must not become their
owner. No lower-level crate should depend on the REPL event loop.

**Where the code lives.** `event_loop.rs` holds the `EventLoop` struct, `new`, and `run`. The
handlers live beside it, grouped by what they handle, and are `pub(super)` so only the loop calls
them:

| File | Handles |
|------|---------|
| `event_loop/dispatch.rs` | the `match event` over every `ReplEvent` variant |
| `event_loop/input.rs` | a submitted line: commands, typed programs, or a query |
| `changeset.rs` | consecutive write/edit/patch grouping and aggregate diff preview |
| `event_loop/tools.rs` | tool results, tool and VM approval prompts |
| `event_loop/brain.rs` | remote and named Brain traffic, invitations, reconnection |
| `event_loop/plan.rs` | plan mode, plan tasks, poset confirmation |
| `event_loop/commands.rs` | slash-style commands: provider switch, feedback, demos |
| `runner_recovery.rs` | leftover-daemon / lease / environment mismatch labels |

**Adding an event.** Add the variant to `ReplEvent` in `events.rs`, then handle it in
`dispatch.rs`. The compiler will tell you the match is non-exhaustive — that is the point of the
enum, so do not add a catch-all arm. Keep the arm short: if handling takes more than a few lines,
put a `pub(super)` method in whichever file above it belongs to and call it.

**IPC recovery is header/status, not transcript.** Peer disconnect, home event-watch loss, and
runner reconnect attempts update `StatusBar` (`SessionLabel`) through
`EventLoop::project_ipc_recovery_header`. They must not call `output_manager.write_info` — that
commits sticky conversation rows between program source and output (#819). Leftover-daemon /
environment mismatch at startup still uses `apply_home_runner_startup` (header plus the detailed
startup TUI line, #794).

**Snapshot replay reconstructs say-turn cards (#970).** The say ViewModel is produced only by the
live paths, so `project_remote_brain_snapshot_runs` also runs
`reconstruct_replayed_say_turn_cards`: a freshly replayed Interactive run whose journal pattern is
one typed `Program` (the run-correlated provider event, or the run-unaffiliated event at
`run.request_seq` for typed programs) plus a successful `Result`, with no tool or approval events,
gets `begin_say_turn` on its run group and the output filled from the Result; the one guarded
`set_complete` transition closes it. The run group's legacy rows stay as the canonical record (a
typed-program replay gains its program row there, since no run-correlated Program event exists),
and the viewport renders the card because the renderer consults `say_turn_view()` first. Runs keep
the legacy projection when the pattern does not match — tools/approvals/speculative prompts, more
than one Program, an errored Result, a failed/cancelled/completed-without-output run, a run unit
this snapshot did not create (a locally rendered live turn or an earlier snapshot), or a unit that
already carries a card (a later snapshot must not reset `show_program` or duplicate output). The
Snapshot branch also skips the run-unaffiliated Program source unit for programs a replayed card
already covers, so one turn never wears two representations. The live event path
(`project_remote_brain_live_run_event`) is untouched.

**The state is shared, and that is the known weakness.** Every handler takes `&mut self` on an
`EventLoop` whose field list is long. Before adding a field, check whether the state belongs to a
query (`query_state.rs`) or to a tool run (`tool_execution.rs`) instead.

**Resume identity.** A clean interactive exit prints `To resume, run: finch attach <brain-name>`
after the home Brain was attached, or a visible persistence failure. It does not mint or
checkpoint a client-owned UUID session file. `ConversationHistory` stays an in-memory
projection; named Brains are the durable store.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- cli::repl_event::`. Tests for the
loop are in `event_loop/tests.rs` and reach private items through `use super::*` exactly as they
did when they were inline.
