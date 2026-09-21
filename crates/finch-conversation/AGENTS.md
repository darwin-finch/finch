# Finch conversation agent contract

Supplements the root [agent rules](../../AGENTS.md). Read the [README](README.md) for ownership
and caller workflows; [`src/lib.rs`](src/lib.rs) is the flat facade and contains the co-located
tests. Do not publish a child-module path or generate an interface catalog.

## Dependencies and extension rules

- Depend on `finch-providers` only for provider-wire `Message` and `ContentBlock` meaning. This
  crate may use standard serialization and filesystem primitives for snapshots; it must not
  import root CLI, provider transports/configuration, Brain storage, tools, runtime, or TUI.
- Keep generation, summarization, tool execution, and checkpoint timing in the root composition
  layer. Add a method here only when it is a rule about committed history or staged tool-round
  state, not because an application caller needs a shortcut to its own dependency.
- Public types used in return values or errors must be named at the crate root. Check real
  cross-crate callers before widening or narrowing visibility.

## Invariants and lifetimes

- A staged assistant payload and partial results are invisible to provider request reads,
  snapshots, compaction input, and serialized JSON. A committed tool round publishes its
  assistant declaration and all matching results together, in declared id order; stale,
  duplicate, unknown, and incomplete results are rejected.
- Context trimming must never expose an orphaned tool-result message as the history prefix.
  The application holds a pre-commit clone until continuation and checkpoint admission succeed;
  this crate can roll the latest complete pair back into staging on failure.
- `save` atomically replaces the JSON file after syncing its bytes, and on Unix syncs the parent
  directory. `load` restores skipped session defaults and starts with no staged rounds. A named
  Brain journal is the durable authority; this snapshot is a client-local projection.

## Focused proof

```bash
./scripts/test_brains.sh cargo test -p finch-conversation --lib
./scripts/test_brains.sh cargo test --lib cli::repl_event::
```

For a crate move or public facade change, run `cargo build --workspace` and the supervised
workspace suite. `scripts/check_subsystems.py` does not exist; use `scripts/seam_cost.py` for
advisory dependency evidence and check relative imports as well.
