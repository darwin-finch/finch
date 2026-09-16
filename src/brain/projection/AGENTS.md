# brain/projection capsule: read-only snapshots, status, and history

Supplements the root [`AGENTS.md`](../../../CLAUDE.md) and the parent
[`brain` capsule](../AGENTS.md), which still apply in full.

**Owns** `src/brain/projection/`: `BrainSnapshot`, client wire messages,
unhydrated list summaries, the environment record carried on snapshots, and
observer-safe effect-audit projection. This facade never appends or mutates.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its
signature. Child modules are private, so the `pub use` list in `mod.rs` is the
whole public surface. Callers outside `src/brain` use `crate::brain::Item`.

**Dependencies:** `journal` for readonly scan and event types; `attachment`,
`run`, and `schedule` for projected records. Do not depend on `BrainStore`
locks or schedule indexing. Do not hydrate, create files, or truncate journals.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- brain::projection::`.
Parent `brain::store::` tests cover unhydrated health counts and snapshot
equality after restart.
