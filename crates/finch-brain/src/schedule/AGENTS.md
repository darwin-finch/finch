# Brain schedule agent contract

Supplements the root [agent rules](../../../../AGENTS.md) and parent
[Brain contract](../../AGENTS.md). The [README](README.md) traces store and server callers;
[`mod.rs`](mod.rs) is the nested facade and [`src/lib.rs`](../../lib.rs) is the external one.

**Owns:** durable schedule and reviewed-initialization vocabulary, delivery policy, the
store-wide `ScheduleIndex`, and due-window arithmetic. The store journals selected due work;
the server and daemon choose when to poll and dispatch it.

**Dependencies:** `attachment` supplies the initiating identity, `run` supplies the queued
`BrainRun` attached to a due event, `journal` supplies `BrainId`, and `finch-vm` supplies the
effect ceiling. This module must not depend on `BrainStore` or root server/daemon/CLI code.
It must not append journal records or acquire runner leases.

**Facade and extension rule:** outside callers use the flat `finch_brain::Item` surface.
`ScheduleIndex` and due-window helpers are for Brain's store, not application code. Add a
crate-level export only for a real caller. Do not publish the child index module or recreate
a generated symbol catalog.

**Invariants and lifetimes:** a due event snapshots source, language, and grant ceiling so a
later schedule edit cannot change already queued work. Coalescing and bounded catch-up have
distinct missed-tick behavior; their arithmetic and stable index order must survive restart.
Reviewed initialization is inert on load and runs only through an explicit scheduled run.
Its module identity and source digest must match the reviewed built-in module; public schedule
creation cannot claim that identity. Do not change due-window or delivery semantics in
documentation-only work.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-brain --lib schedule::` for
due arithmetic and index ordering. Run supervised `store::` tests for index warm-up after
restart, prune races, and archival versus delivery when scheduling behavior changes; run
supervised server Brain-service tests when changing an external schedule contract.
