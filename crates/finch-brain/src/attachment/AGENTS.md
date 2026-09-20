# brain/attachment capsule: participant projections and reconnect cursors

Supplements the root [`AGENTS.md`](../../CLAUDE.md) and the parent
[`brain` capsule](../../AGENTS.md), which still apply in full.

**Owns** `crates/finch-brain/src/attachment/`: attachment and connection identities, participant
records, approval audience, and the durable `attachments.json` cursor file used
to materialize a client's projection across reconnect. The journal records
attach/detach; this facade does not append events.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its
signature. Child modules are private, so the `pub use` list in `mod.rs` is the
whole nested surface. Callers outside the crate use the flat `finch_brain::Item` facade.

**Dependencies:** `journal` for `BrainId` and durable directory creation. Do not
depend on `schedule`, `run` orchestration, or `BrainStore`. Do not change cursor
file layout or reconnect semantics in a facade-only commit.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-brain --lib attachment::`.
Parent `store::` tests cover attach/detach races and stale-connection
disconnect. Run those when changing a re-exported type.
