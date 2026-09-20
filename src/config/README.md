# config: configuration, instructions, licensing, metrics

This is Finch's settings and identity layer: provider entries, personas, credential resolvers,
backend selection, notice-state bookkeeping, project instruction loading, licensing, and usage
metrics. It exists as one subsystem because nearly every other subsystem reads configuration, so
its load order, secret-handling, and atomic-save rules need one authoritative home rather than
being re-decided per caller.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md).

## Further documentation

- [`CONFIGURATION.md`](CONFIGURATION.md) — the user-facing configuration contract.
- [`../license/LICENSING.md`](../license/LICENSING.md) — the license system this capsule owns.
- [`../context/README.md`](../context/README.md) — project instruction loading, split out as its
  own facade.
