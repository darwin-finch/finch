# brain/journal capsule: append-only events and replay

Supplements the root [`AGENTS.md`](../../../../AGENTS.md) and the parent
[`brain` capsule](../../AGENTS.md), which still apply in full.

**Owns** `crates/finch-brain/src/journal/`: the durable event envelope, mutation receipts,
JSONL persist (single events, checksummed batches, atomic rewrite), torn-tail
and corrupt-tail recovery, metadata identity, schema constants, and legacy
speculative-run correlation backfill. The log is authoritative.

**Boundary:** the [README](README.md) traces store commits and read-only listing. The nested
[`mod.rs`](mod.rs) owns event and persistence contracts; callers outside the crate use the
flat [`finch-brain` facade](../lib.rs). Do not recreate a generated signature catalog.

**Dependencies:** `attachment`, `run`, and `schedule` for event payload types
only. Do not select due work, acquire leases, or mutate attachment cursors.
Do not change journal framing, checksums, or sequence recovery in a
facade-only commit. Consume the Runtime/Application ABI for effect-audit
payloads; do not copy delivery.

**Invariants and lifetimes:** a checksummed batch either replays in full or not at all after a
torn append. A receipt belongs to the first canonical event for one authorized mutation;
retry resolution must not append a second transition. Read-only scans must not hydrate,
create files, or repair the log. The store controls writer lifetime and authorization.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-brain --lib journal::`.
Parent `store::` tests cover hydration replay, effect-audit sequence
holds, and mutation idempotence through `BrainStore`.
