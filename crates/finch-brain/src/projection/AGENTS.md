# brain/projection capsule: read-only snapshots, status, and history

Supplements the root [`AGENTS.md`](../../../../AGENTS.md) and the parent
[`brain` capsule](../../AGENTS.md), which still apply in full.

**Owns** `crates/finch-brain/src/projection/`: `BrainSnapshot`, client wire messages,
unhydrated list summaries, the environment record carried on snapshots, and
observer-safe effect-audit projection. This facade never appends or mutates.

**Boundary:** the [README](README.md) traces an unhydrated store listing and a REPL
runner-handoff decision. [`mod.rs`](mod.rs) owns the nested snapshot and summary surface;
external crates use the flat [`finch-brain` facade](../lib.rs). Do not recreate a generated
signature catalog.

**Dependencies:** `journal` for readonly scan and event types; `attachment`,
`run`, and `schedule` for projected records. Do not depend on `BrainStore`
locks or schedule indexing. Do not hydrate, create files, or truncate journals.

**Invariants and lifetimes:** `BrainSnapshot` is a projection of committed events, not a
second authority for runner leases. An unhydrated list may read existing metadata and journal
files but must not create, repair, or truncate them. Observer effect-audit events must not
disclose fields withheld from observers. Keep handoff detection tied to the exact lease id.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-brain --lib projection::`.
Parent `store::` tests cover unhydrated health counts and snapshot
equality after restart.
