# repl_event: the interactive REPL event loop

This is the machinery behind the interactive REPL: a single Tokio `select!` that reads user input,
provider output, and Brain traffic, turns each into a `ReplEvent`, and dispatches it to one of a
handful of handler modules grouped by what they handle (tools, Brain traffic, plan mode, commands).
It exists as one loop with an exhaustive match — rather than scattered ad hoc event handling — so
adding a new kind of event is a compiler-enforced, one-place change.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).

## Further documentation

[`ATOMIC_HISTORY.md`](ATOMIC_HISTORY.md) — how scrollback commits stay exactly-once across
resizes and retries.
