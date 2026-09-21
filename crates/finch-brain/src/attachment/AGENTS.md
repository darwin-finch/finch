# brain/attachment capsule: participant projections and reconnect cursors

Supplements the root [`AGENTS.md`](../../../../AGENTS.md) and the parent
[`brain` capsule](../../AGENTS.md), which still apply in full.

**Owns** `crates/finch-brain/src/attachment/`: attachment and connection identities, participant
records, approval audience, and the durable `attachments.json` cursor file used
to materialize a client's projection across reconnect. The journal records
attach/detach; this facade does not append events.

**Boundary:** the [README](README.md) traces store and journal-replay workflows.
[`mod.rs`](mod.rs) owns the nested types and cursor operations; callers outside the crate use
the flat [`finch-brain` facade](../lib.rs). Do not re-create a signature catalog.

**Dependencies:** `journal` for `BrainId`, attach/detach events, and durable directory creation.
Do not depend on `schedule`, `run` orchestration, or `BrainStore`. An attachment id can survive
a connection replacement; an old connection's detach must not clear the new one. Cursor files
must reject a mismatched Brain identity instead of silently rewinding. Do not change cursor file
layout or reconnect semantics in a facade-only commit.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-brain --lib attachment::`.
Parent `store::` tests cover attach/detach races and stale-connection
disconnect. Run those when changing a re-exported type.
