# Finch iterative planning

This module owns the `/plan` IMPCPD loop: draft a numbered plan, select critique personas,
parse and score their concerns, ask for steering, and stop on approval, convergence, cancellation,
or the iteration cap. It owns the embedded methodology text and the typed results of one loop.
It does not own REPL mode, plan-file persistence, provider transport, or terminal painting.

For `/plan <task>`, the interactive event loop in `src/cli/repl_event/event_loop/plan.rs`
constructs `PlanLoop` from the selected `Generator`, `OutputManager`, and `ImpcpdConfig`, then
passes the shared TUI renderer to `run`. The event loop owns mode transitions, the timestamped
plan path, and final execution confirmation; this module owns draft/critique/steering iterations
and returns a `PlanResult` for the caller to interpret.

The ignored live contract tests in `tests/live/impcpd.rs` are another caller. They send
`IMPCPD_METHODOLOGY` with a sample plan to real configured providers and parse the response as
`CritiqueItem` values. That proves a provider follows the prompt schema; it does not exercise
the REPL mode or imply every provider passes without dated live evidence.

Read [AGENTS.md](AGENTS.md) for dependency and test rules, [mod.rs](mod.rs) for the flat facade,
and [impcpd_methodology.md](impcpd_methodology.md) only when changing critique semantics.
