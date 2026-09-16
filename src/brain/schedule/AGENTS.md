# brain/schedule capsule: schedules and due-work selection

Supplements the root [`AGENTS.md`](../../../CLAUDE.md) and the parent
[`brain` capsule](../AGENTS.md), which still apply in full.

**Owns** `src/brain/schedule/`: schedule and initialization records, delivery
policy, the store-wide due index (`ScheduleIndex`), and due-window arithmetic.
The daemon selects work by due time and hydrates only named Brains.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its
signature. Child modules are private, so the `pub use` list in `mod.rs` is the
whole public surface. Callers outside `src/brain` use `crate::brain::Item`.

**Dependencies:** `attachment` and `run` for initiating identity and the queued
`BrainRun` snapshot on a due event; `journal` for `BrainId` on the
initialization contract. Do not append journal records or acquire runner leases
here. Do not change due-window or coalesce/catch-up semantics in a facade-only
commit.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- brain::schedule::`.
Parent `brain::store::` tests cover index warm-up after restart, prune races,
and archival vs delivery.
