# repl_event capsule: the interactive REPL event loop

Supplements the root [`AGENTS.md`](../../../CLAUDE.md), which still applies in full.

**What this is.** The machinery behind the interactive REPL. A Tokio `select!` in
`EventLoop::run` reads user input, provider output, and Brain traffic, turns each into a
`ReplEvent`, and dispatches it. Everything here is driven by that one loop; nothing here talks to a
provider or a tool directly. Tool execution goes through [`crate::tools::ToolLoop`]
(shared with the scheduler): stream events are observed, then admitted at most
once.

**Where the code lives.** `event_loop.rs` holds the `EventLoop` struct, `new`, and `run`. The
handlers live beside it, grouped by what they handle, and are `pub(super)` so only the loop calls
them:

| File | Handles |
|------|---------|
| `event_loop/dispatch.rs` | the `match event` over every `ReplEvent` variant |
| `event_loop/input.rs` | a submitted line: commands, typed programs, or a query |
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

**The state is shared, and that is the known weakness.** Every handler takes `&mut self` on an
`EventLoop` whose field list is long. Before adding a field, check whether the state belongs to a
query (`query_state.rs`) or to a tool run (`tool_execution.rs`) instead.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- cli::repl_event::`. Tests for the
loop are in `event_loop/tests.rs` and reach private items through `use super::*` exactly as they
did when they were inline.
