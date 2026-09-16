# brain/journal capsule: append-only events and replay

Supplements the root [`AGENTS.md`](../../../CLAUDE.md) and the parent
[`brain` capsule](../AGENTS.md), which still apply in full.

**Owns** `src/brain/journal/`: the durable event envelope, mutation receipts,
JSONL persist (single events, checksummed batches, atomic rewrite), torn-tail
and corrupt-tail recovery, metadata identity, schema constants, and legacy
speculative-run correlation backfill. The log is authoritative.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its
signature. Child modules are private, so the `pub use` list in `mod.rs` is the
whole public surface. Callers outside `src/brain` use `crate::brain::Item`.

**Dependencies:** `attachment`, `run`, and `schedule` for event payload types
only. Do not select due work, acquire leases, or mutate attachment cursors.
Do not change journal framing, checksums, or sequence recovery in a
facade-only commit. Consume the Runtime/Application ABI for effect-audit
payloads; do not copy delivery.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- brain::journal::`.
Parent `brain::store::` tests cover hydration replay, effect-audit sequence
holds, and mutation idempotence through `BrainStore`.
