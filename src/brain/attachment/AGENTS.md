# brain/attachment capsule: participant projections and reconnect cursors

Supplements the root [`AGENTS.md`](../../../CLAUDE.md) and the parent
[`brain` capsule](../AGENTS.md), which still apply in full.

**Owns** `src/brain/attachment/`: attachment and connection identities, participant
records, approval audience, and the durable `attachments.json` cursor file used
to materialize a client's projection across reconnect. The journal records
attach/detach; this facade does not append events.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its
signature. Child modules are private, so the `pub use` list in `mod.rs` is the
whole public surface. Callers outside `src/brain` use `crate::brain::Item`.

**Dependencies:** `journal` for `BrainId` and durable directory creation. Do not
depend on `schedule`, `run` orchestration, or `BrainStore`. Do not change cursor
file layout or reconnect semantics in a facade-only commit.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- brain::attachment::`.
Parent `brain::store::` tests cover attach/detach races and stale-connection
disconnect. Run those when changing a re-exported type.
